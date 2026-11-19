use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use askama::Template;
use rouille::{Request, Response, post_input, router};

use crate::alldebrid::{Magnet, MagnetFile};
use crate::aria2::{self, Download};
use crate::media::{self, Classification, Decision, Kind};
use crate::{
    AppState, CachedFiles, SendState, SentMagnet, Settings, normalize_aria2_url, normalize_prefix,
    unix_now,
};

/// Most magnet IDs sent in one `magnet/files` request.
const FILES_BATCH_SIZE: usize = 50;

/// Magnets per page on the home page.
const PAGE_SIZE: usize = 10;

/// Pause between `link/unlock` requests, to stay well under AllDebrid's
/// limit of 12 requests per second.
const UNLOCK_INTERVAL: Duration = Duration::from_millis(100);

/// Entry point for every HTTP request.
pub fn handle(request: &Request, state: &AppState) -> Response {
    router!(request,
        (GET) ["/"] => { magnets_page(request, state, View::All) },
        (GET) ["/latest"] => { magnets_page(request, state, View::Latest) },
        (GET) ["/settings"] => { settings_page(request, state) },
        (POST) ["/settings"] => { save_settings(request, state) },
        (POST) ["/magnets/{id}/name", id: u64] => { set_name(request, state, id) },
        (POST) ["/magnets/{id}/send", id: u64] => { send_magnet(request, state, id) },
        _ => Response::empty_404()
    )
}

// ---------- Magnet lists ----------

/// Which magnets a list page shows.
#[derive(Clone, Copy, PartialEq)]
enum View {
    /// Home: every magnet on the account, newest first, a page at a time.
    All,
    /// Latest: magnets added since `AppState::since`, all on one page.
    Latest,
}

#[derive(Template)]
#[template(path = "magnets.html")]
struct MagnetsTemplate {
    /// The Latest tab rather than the paged list of every magnet.
    latest: bool,
    /// This page's path, which its forms send so the reply comes back here.
    back: String,
    has_api_key: bool,
    error: Option<String>,
    /// How long ago "new" starts, e.g. "5 min ago".
    since_ago: String,
    /// Paging links, on the home page.
    pager: Option<Pager>,
    aria2_configured: bool,
    /// Why download progress couldn't be fetched from aria2.
    aria2_error: Option<String>,
    magnets: Vec<MagnetCard>,
}

struct Pager {
    /// Page shown, from 1.
    page: usize,
    /// Links to the newer and older pages, if there are any.
    prev: Option<String>,
    next: Option<String>,
    /// Positions of the first and last magnets shown (from 1), and how many there are.
    first: usize,
    last: usize,
    total: usize,
}

impl Pager {
    /// Page `requested` of `total` items, clamped to the pages that exist.
    fn new(total: usize, requested: usize, size: usize) -> Self {
        let pages = total.div_ceil(size).max(1);
        let page = requested.clamp(1, pages);
        let start = (page - 1) * size;
        Pager {
            page,
            prev: (page > 1).then(|| page_path(page - 1)),
            next: (page < pages).then(|| page_path(page + 1)),
            first: (start + 1).min(total),
            last: (start + size).min(total),
            total,
        }
    }

    /// Indexes of this page's items.
    fn range(&self) -> std::ops::Range<usize> {
        self.first.saturating_sub(1)..self.last
    }
}

/// Path of a page of the home list.
fn page_path(page: usize) -> String {
    if page <= 1 {
        "/".to_string()
    } else {
        format!("/?page={page}")
    }
}

struct MagnetCard {
    id: u64,
    name: String,
    size: String,
    status: String,
    status_class: &'static str,
    added: String,
    ready: bool,
    /// Why the file list couldn't be loaded, for a ready magnet.
    files_error: Option<String>,
    /// TV / Movie / Other, once the files are known.
    kind: Option<&'static str>,
    /// The editable show name or movie title, for TV and movies not yet sent.
    media_name: Option<MediaName>,
    /// Distinct destination folders of the included files.
    destinations: Vec<String>,
    files: Vec<FileRow>,
    /// TV or movie: the kinds that can be sent to aria2.
    is_media: bool,
    /// Number of included files, which is what gets sent to aria2.
    send_count: usize,
    /// Whether the "Send to aria2" button is shown.
    sendable: bool,
    /// Being sent right now (by another request).
    sending: bool,
    /// Why the last attempt to send failed.
    send_error: Option<String>,
    /// Summary of what was sent and how the downloads are doing.
    sent: Option<String>,
}

struct MediaName {
    /// "Show name" or "Movie title".
    label: &'static str,
    /// The name in use: the user's override, or else the detected one.
    current: String,
    detected: String,
    overridden: bool,
}

struct FileRow {
    /// Path inside the torrent, without the top folder when that's just the magnet name.
    path: String,
    size: String,
    dest_dir: Option<String>,
    /// New file name, when it differs from the original.
    renamed_to: Option<String>,
    excluded: Option<&'static str>,
    /// aria2's progress for the file, once sent.
    download: Option<String>,
    /// CSS class for `download`: `ready`, `active`, `error` or `muted`.
    download_class: &'static str,
}

/// aria2's status for each GID, or why it couldn't be fetched (e.g. "GID not found").
type Downloads = HashMap<String, Result<aria2::Status, String>>;

fn magnets_page(request: &Request, state: &AppState, view: View) -> Response {
    // Copy the settings so the lock isn't held during HTTP requests.
    let s = settings(state).clone();
    let now = unix_now();
    let requested_page = request
        .get_param("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);

    let mut page = MagnetsTemplate {
        latest: view == View::Latest,
        back: match view {
            View::All => page_path(requested_page),
            View::Latest => "/latest".to_string(),
        },
        has_api_key: s.alldebrid_api_key.is_some(),
        error: None,
        since_ago: format_ago(now - state.since),
        pager: None,
        aria2_configured: s.aria2_url.is_some(),
        aria2_error: None,
        magnets: Vec::new(),
    };
    let Some(key) = &s.alldebrid_api_key else {
        return render(&page);
    };

    let mut magnets = match state.alldebrid.magnets(key) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("failed to list magnets: {e}");
            page.error = Some(e.to_string());
            return render(&page);
        }
    };
    // Newest first; the ID breaks ties so pages don't shuffle between loads.
    magnets.sort_by_key(|m| std::cmp::Reverse((m.upload_date, m.id)));
    match view {
        View::All => {
            let pager = Pager::new(magnets.len(), requested_page, PAGE_SIZE);
            magnets = magnets.drain(pager.range()).collect();
            page.back = page_path(pager.page);
            page.pager = Some(pager);
        }
        View::Latest => magnets.retain(|m| m.upload_date >= state.since),
    }

    let file_errors = fetch_missing_files(state, key, &magnets);
    let (downloads, aria2_error) = fetch_download_statuses(state, &s, &magnets);
    page.aria2_error = aria2_error;

    let cache = lock(&state.files);
    let overrides = lock(&state.name_overrides);
    let sends = lock(&state.sends);
    page.magnets = magnets
        .iter()
        .map(|m| {
            let files = cache.get(&m.id).map(|c| c.files.as_slice());
            let error = file_errors.get(&m.id).cloned();
            let name_override = overrides.get(&m.id).map(String::as_str);
            let send = sends.get(&m.id);
            let mut card = magnet_card(m, files, error, name_override, &s.dest_prefix, now);
            if card.ready {
                add_send_info(&mut card, m, send, &downloads, now);
            }
            card
        })
        .collect();
    drop(sends);
    drop(overrides);
    drop(cache);

    render(&page)
}

/// Fetches file lists for ready magnets that aren't cached yet, and caches
/// them. Returns an error message for each magnet whose list couldn't be fetched.
fn fetch_missing_files(state: &AppState, key: &str, magnets: &[Magnet]) -> HashMap<u64, String> {
    let missing: Vec<&Magnet> = {
        let cache = lock(&state.files);
        magnets
            .iter()
            .filter(|m| m.is_ready() && !cache.contains_key(&m.id))
            .collect()
    };

    let mut errors = HashMap::new();
    for batch in missing.chunks(FILES_BATCH_SIZE) {
        let ids: Vec<u64> = batch.iter().map(|m| m.id).collect();
        log::info!("fetching file lists for {} magnet(s)", ids.len());
        match state.alldebrid.magnet_files(key, &ids) {
            Ok(results) => {
                let mut cache = lock(&state.files);
                for (id, result) in results {
                    let Some(magnet) = batch.iter().find(|m| m.id == id) else {
                        continue;
                    };
                    match result {
                        Ok(files) => {
                            let magnet_name = magnet.filename.clone();
                            cache.insert(id, CachedFiles { magnet_name, files });
                        }
                        Err(e) => {
                            errors.insert(id, format!("{} ({})", e.message, e.code));
                        }
                    }
                }
                for id in ids {
                    if !cache.contains_key(&id) && !errors.contains_key(&id) {
                        errors.insert(id, "AllDebrid returned no file list".to_string());
                    }
                }
            }
            Err(e) => {
                log::warn!("failed to fetch file lists: {e}");
                for id in ids {
                    errors.insert(id, e.to_string());
                }
            }
        }
    }
    errors
}

/// Asks aria2 how the downloads of the shown, sent magnets are doing, in one
/// request. Returns the statuses by GID, and an error if aria2 couldn't be asked.
fn fetch_download_statuses(
    state: &AppState,
    settings: &Settings,
    magnets: &[Magnet],
) -> (Downloads, Option<String>) {
    let gids: Vec<String> = {
        let sends = lock(&state.sends);
        magnets
            .iter()
            .filter_map(|m| match sends.get(&m.id) {
                Some(SendState::Sent(sent)) => Some(sent.results.values()),
                _ => None,
            })
            .flatten()
            .filter_map(|r| r.as_ref().ok().cloned())
            .collect()
    };
    let Some(endpoint) = settings.aria2_endpoint() else {
        return (HashMap::new(), None);
    };
    if gids.is_empty() {
        return (HashMap::new(), None);
    }

    let gid_refs: Vec<&str> = gids.iter().map(String::as_str).collect();
    match state.aria2.statuses(&endpoint, &gid_refs) {
        Ok(statuses) => {
            let downloads = gids
                .into_iter()
                .zip(statuses)
                .map(|(gid, status)| (gid, status.map_err(|e| e.message)))
                .collect();
            (downloads, None)
        }
        Err(e) => {
            log::warn!("failed to get download statuses from aria2: {e}");
            (HashMap::new(), Some(e.to_string()))
        }
    }
}

fn magnet_card(
    m: &Magnet,
    files: Option<&[MagnetFile]>,
    files_error: Option<String>,
    name_override: Option<&str>,
    prefix: &str,
    now: i64,
) -> MagnetCard {
    let status_class = if m.is_ready() {
        "ready"
    } else if m.is_error() {
        "error"
    } else {
        "active"
    };
    MagnetCard {
        id: m.id,
        name: m.filename.clone(),
        size: format_size(m.size),
        status: m.status.clone(),
        status_class,
        added: format_ago(now - m.upload_date),
        ready: m.is_ready(),
        files_error,
        kind: None,
        media_name: None,
        destinations: Vec::new(),
        files: Vec::new(),
        is_media: false,
        send_count: 0,
        sendable: false,
        sending: false,
        send_error: None,
        sent: None,
    }
    .with_files(m, files, name_override, prefix)
}

impl MagnetCard {
    /// Fills in the kind, name and file rows from a fresh classification.
    fn with_files(
        mut self,
        m: &Magnet,
        files: Option<&[MagnetFile]>,
        name_override: Option<&str>,
        prefix: &str,
    ) -> Self {
        let Some(files) = files.filter(|_| m.is_ready()) else {
            return self;
        };
        let classification = media::classify(&m.filename, files, prefix, name_override);
        self.media_name = classification.detected_name.clone().map(|detected| MediaName {
            label: match classification.kind {
                Kind::Movie => "Movie title",
                _ => "Show name",
            },
            current: name_override.unwrap_or(&detected).to_string(),
            overridden: name_override.is_some_and(|n| n != detected),
            detected,
        });
        self.set_files(&m.filename, classification);
        self
    }

    /// Fills in the kind, destinations and file rows from a classification.
    fn set_files(&mut self, magnet_name: &str, classification: Classification) {
        self.kind = Some(classification.kind.label());
        self.is_media = matches!(classification.kind, Kind::Tv | Kind::Movie);
        let top_folder = format!("{magnet_name}/");
        let mut destinations = BTreeSet::new();
        self.files = classification
            .files
            .into_iter()
            .map(|planned| {
                let original_name = planned.file.path.rsplit('/').next().unwrap_or_default();
                let path = planned
                    .file
                    .path
                    .strip_prefix(&top_folder)
                    .unwrap_or(&planned.file.path)
                    .to_string();
                let mut row = FileRow {
                    path,
                    size: format_size(planned.file.size),
                    dest_dir: None,
                    renamed_to: None,
                    excluded: None,
                    download: None,
                    download_class: "muted",
                };
                match planned.decision {
                    Decision::Include {
                        dest_dir,
                        file_name,
                    } => {
                        if file_name != original_name {
                            row.renamed_to = Some(file_name);
                        }
                        destinations.insert(dest_dir.clone());
                        row.dest_dir = Some(dest_dir);
                    }
                    Decision::Exclude { reason } => row.excluded = Some(reason),
                }
                row
            })
            .collect();
        self.send_count = self.files.iter().filter(|f| f.excluded.is_none()).count();
        self.destinations = destinations.into_iter().collect();
    }
}

/// Adds the aria2 part of a ready magnet's card: the send button, the
/// result of the last attempt, or the progress of the downloads.
fn add_send_info(
    card: &mut MagnetCard,
    m: &Magnet,
    send: Option<&SendState>,
    downloads: &Downloads,
    now: i64,
) {
    let can_send = card.is_media && card.send_count > 0;
    match send {
        None => card.sendable = can_send,
        Some(SendState::Failed { error }) => {
            card.sendable = can_send;
            card.send_error = Some(error.clone());
        }
        Some(SendState::Sending) => {
            card.sending = true;
            card.media_name = None;
        }
        Some(SendState::Sent(sent)) => {
            // Show the destinations as sent, not as they'd be now.
            card.media_name = None;
            card.set_files(&m.filename, sent.classification.clone());
            for (row, planned) in card.files.iter_mut().zip(&sent.classification.files) {
                if let Some(result) = sent.results.get(&planned.file.path) {
                    let (label, class) = download_label(result, downloads);
                    row.download = Some(label);
                    row.download_class = class;
                }
            }
            card.sent = Some(sent_summary(sent, downloads, now));
        }
    }
}

/// One line of status for a sent file.
fn download_label(result: &Result<String, String>, downloads: &Downloads) -> (String, &'static str) {
    let gid = match result {
        Ok(gid) => gid,
        Err(e) => return (format!("aria2 refused it: {e}"), "error"),
    };
    match downloads.get(gid) {
        None => (format!("Sent to aria2 (GID {gid})"), "muted"),
        Some(Err(_)) => ("No longer in aria2".to_string(), "muted"),
        Some(Ok(s)) => match s.status.as_str() {
            "complete" => ("Downloaded".to_string(), "ready"),
            "active" => {
                let mut label = format!("Downloading {}%", percent(s.completed_length, s.total_length));
                if s.download_speed > 0 {
                    label.push_str(&format!(" · {}/s", format_size(s.download_speed)));
                }
                (label, "active")
            }
            "waiting" => ("Queued in aria2".to_string(), "muted"),
            "paused" => ("Paused in aria2".to_string(), "muted"),
            "removed" => ("Removed from aria2".to_string(), "error"),
            "error" => {
                let msg = s.error_message.as_deref().unwrap_or("unknown error");
                (format!("Download failed: {msg}"), "error")
            }
            other => (other.to_string(), "muted"),
        },
    }
}

/// e.g. "Sent to aria2 5 min ago · 2 of 6 files downloaded · 1.2 GB of 4.0 GB (30%) · 5.0 MB/s".
fn sent_summary(sent: &SentMagnet, downloads: &Downloads, now: i64) -> String {
    let gids: Vec<&String> = sent.results.values().filter_map(|r| r.as_ref().ok()).collect();
    let refused = sent.results.len() - gids.len();
    let statuses: Vec<&aria2::Status> = gids
        .iter()
        .filter_map(|gid| downloads.get(*gid)?.as_ref().ok())
        .collect();

    let mut parts = vec![format!("Sent to aria2 {}", format_ago(now - sent.at))];
    if !statuses.is_empty() {
        let complete = statuses.iter().filter(|s| s.status == "complete").count();
        parts.push(format!("{complete} of {} {} downloaded", gids.len(), plural(gids.len(), "file")));
        let total: u64 = statuses.iter().map(|s| s.total_length).sum();
        let done: u64 = statuses.iter().map(|s| s.completed_length).sum();
        if total > 0 {
            parts.push(format!(
                "{} of {} ({}%)",
                format_size(done),
                format_size(total),
                percent(done, total)
            ));
        }
        let speed: u64 = statuses.iter().map(|s| s.download_speed).sum();
        if speed > 0 {
            parts.push(format!("{}/s", format_size(speed)));
        }
    }
    if refused > 0 {
        parts.push(format!("{refused} {} refused by aria2", plural(refused, "file")));
    }
    parts.join(" · ")
}

/// Sets or clears the show name / movie title used for one magnet.
fn set_name(request: &Request, state: &AppState, id: u64) -> Response {
    let form = match post_input!(request, {
        name: Option<String>,
        action: Option<String>,
        back: Option<String>,
    }) {
        Ok(form) => form,
        Err(_) => return Response::text("Bad form data").with_status_code(400),
    };

    let mut overrides = lock(&state.name_overrides);
    let name = match form.action.as_deref() {
        Some("reset") => None,
        _ => media::clean_name_override(&form.name.unwrap_or_default()),
    };
    match name {
        Some(name) => {
            log::info!("magnet {id}: name set to {name}");
            overrides.insert(id, name);
        }
        None => {
            log::info!("magnet {id}: name reset to detected");
            overrides.remove(&id);
        }
    }
    back_to_card(form.back.as_deref(), id)
}

// ---------- Sending to aria2 ----------

/// Sends a magnet's included files to aria2, once.
fn send_magnet(request: &Request, state: &AppState, id: u64) -> Response {
    let form = post_input!(request, { back: Option<String> }).ok();
    let back = back_to_card(form.and_then(|f| f.back).as_deref(), id);

    // Claim the magnet, so a double click or a second tab can't send it twice.
    {
        let mut sends = lock(&state.sends);
        if matches!(sends.get(&id), Some(SendState::Sending | SendState::Sent(_))) {
            log::info!("magnet {id}: already sent, or being sent");
            return back;
        }
        sends.insert(id, SendState::Sending);
    }

    let s = settings(state).clone();
    let outcome = match send_files(state, &s, id) {
        Ok(sent) => SendState::Sent(sent),
        Err(error) => {
            log::warn!("magnet {id}: not sent: {error}");
            SendState::Failed { error }
        }
    };
    lock(&state.sends).insert(id, outcome);
    back
}

/// Unlocks the links of the included files and adds them to aria2. If any
/// link can't be unlocked, or aria2 can't be reached, nothing is sent.
fn send_files(state: &AppState, s: &Settings, id: u64) -> Result<SentMagnet, String> {
    let key = s
        .alldebrid_api_key
        .as_deref()
        .ok_or("No AllDebrid API key is set.")?;
    let endpoint = s
        .aria2_endpoint()
        .ok_or("The aria2 RPC URL isn't set. Add it in Settings.")?;

    // Classify again, so what's sent matches what the page showed.
    let classification = {
        let cache = lock(&state.files);
        let cached = cache
            .get(&id)
            .ok_or("The file list isn't loaded. Reload the page and try again.")?;
        let overrides = lock(&state.name_overrides);
        let name_override = overrides.get(&id).map(String::as_str);
        media::classify(&cached.magnet_name, &cached.files, &s.dest_prefix, name_override)
    };
    if !matches!(classification.kind, Kind::Tv | Kind::Movie) {
        return Err("Only TV and movie magnets can be sent.".to_string());
    }
    let included: Vec<(&MagnetFile, &str, &str)> = classification
        .files
        .iter()
        .filter_map(|p| match &p.decision {
            Decision::Include {
                dest_dir,
                file_name,
            } => Some((&p.file, dest_dir.as_str(), file_name.as_str())),
            Decision::Exclude { .. } => None,
        })
        .collect();
    if included.is_empty() {
        return Err("No files are included.".to_string());
    }

    // Check aria2 first, so a wrong URL or secret doesn't waste unlock requests.
    state
        .aria2
        .version(&endpoint)
        .map_err(|e| e.to_string())?;

    log::info!("magnet {id}: unlocking {} link(s)", included.len());
    let mut urls = Vec::with_capacity(included.len());
    for (i, (file, _, _)) in included.iter().enumerate() {
        if file.link.is_empty() {
            return Err(format!("{} has no AllDebrid link.", file.path));
        }
        if i > 0 {
            std::thread::sleep(UNLOCK_INTERVAL);
        }
        let url = state
            .alldebrid
            .unlock_link(key, &file.link)
            .map_err(|e| format!("Could not unlock {}: {e}", file.path))?;
        urls.push(url);
    }

    let downloads: Vec<Download> = included
        .iter()
        .zip(&urls)
        .map(|((_, dir, out), url)| Download { url, dir, out })
        .collect();
    let results = state
        .aria2
        .add_uris(&endpoint, &downloads)
        .map_err(|e| e.to_string())?;

    let mut by_path = HashMap::new();
    let mut results = results.into_iter();
    for (file, _, _) in &included {
        let result = match results.next() {
            Some(Ok(gid)) => Ok(gid),
            Some(Err(e)) => Err(e.message),
            None => Err("aria2 gave no reply for this file".to_string()),
        };
        by_path.insert(file.path.clone(), result);
    }
    let added = by_path.values().filter(|r| r.is_ok()).count();
    log::info!("magnet {id}: added {added} of {} file(s) to aria2", by_path.len());

    Ok(SentMagnet {
        at: unix_now(),
        classification,
        results: by_path,
    })
}

// ---------- Settings ----------

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    masked_key: Option<String>,
    username: Option<String>,
    dest_prefix: String,
    /// The aria2 RPC URL, or empty if not set.
    aria2_url: String,
    aria2_has_secret: bool,
    /// aria2's version, if it answered.
    aria2_version: Option<String>,
    /// Why aria2 couldn't be reached.
    aria2_error: Option<String>,
    saved: bool,
    error: Option<String>,
}

impl SettingsTemplate {
    /// Builds the page, checking the connection to aria2 if it's set up.
    fn new(state: &AppState) -> Self {
        let s = settings(state).clone();
        let (aria2_version, aria2_error) = match s.aria2_endpoint() {
            Some(endpoint) => match state.aria2.version(&endpoint) {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e.to_string())),
            },
            None => (None, None),
        };
        Self {
            masked_key: s.alldebrid_api_key.as_deref().map(mask_key),
            username: s.alldebrid_username.clone(),
            dest_prefix: s.dest_prefix.clone(),
            aria2_url: s.aria2_url.clone().unwrap_or_default(),
            aria2_has_secret: s.aria2_secret.is_some(),
            aria2_version,
            aria2_error,
            saved: false,
            error: None,
        }
    }
}

fn settings_page(request: &Request, state: &AppState) -> Response {
    let mut page = SettingsTemplate::new(state);
    page.saved = request.get_param("saved").is_some();
    render(&page)
}

fn save_settings(request: &Request, state: &AppState) -> Response {
    let form = match post_input!(request, {
        alldebrid_api_key: Option<String>,
        dest_prefix: Option<String>,
        aria2_url: Option<String>,
        aria2_secret: Option<String>,
        action: Option<String>,
    }) {
        Ok(form) => form,
        Err(_) => return Response::text("Bad form data").with_status_code(400),
    };

    match form.action.as_deref() {
        Some("save_key") => save_key(state, form.alldebrid_api_key.unwrap_or_default()),
        Some("remove_key") => {
            let mut s = settings(state);
            s.alldebrid_api_key = None;
            s.alldebrid_username = None;
            log::info!("AllDebrid API key removed");
            Response::redirect_303("/settings?saved")
        }
        Some("save_prefix") => {
            match normalize_prefix(&form.dest_prefix.unwrap_or_default()) {
                Some(prefix) => {
                    log::info!("destination prefix set to {prefix}");
                    settings(state).dest_prefix = prefix;
                    Response::redirect_303("/settings?saved")
                }
                None => settings_error(state, "Enter a destination prefix to save.".to_string()),
            }
        }
        Some("save_aria2") => {
            match normalize_aria2_url(&form.aria2_url.unwrap_or_default()) {
                Ok(url) => {
                    log::info!("aria2 RPC URL set to {url}");
                    let mut s = settings(state);
                    s.aria2_url = Some(url);
                    // A blank secret field keeps the current secret.
                    let secret = form.aria2_secret.unwrap_or_default();
                    if !secret.trim().is_empty() {
                        log::info!("aria2 RPC secret changed");
                        s.aria2_secret = Some(secret.trim().to_string());
                    }
                    // The settings page checks the connection when it loads.
                    Response::redirect_303("/settings?saved")
                }
                Err(e) => settings_error(state, e.to_string()),
            }
        }
        Some("remove_aria2") => {
            let mut s = settings(state);
            s.aria2_url = None;
            s.aria2_secret = None;
            log::info!("aria2 settings removed");
            Response::redirect_303("/settings?saved")
        }
        _ => Response::text("Unknown action").with_status_code(400),
    }
}

fn save_key(state: &AppState, key: String) -> Response {
    let key = key.trim();
    if key.is_empty() {
        return settings_error(state, "Enter an API key to save.".to_string());
    }

    // Check the key against AllDebrid before storing it.
    match state.alldebrid.user(key) {
        Ok(user) => {
            log::info!("AllDebrid API key saved for user {}", user.username);
            let mut s = settings(state);
            s.alldebrid_api_key = Some(key.to_string());
            s.alldebrid_username = Some(user.username);
            Response::redirect_303("/settings?saved")
        }
        Err(e) => settings_error(state, format!("The API key was not saved: {e}")),
    }
}

/// Re-renders the settings page with an error, leaving the stored settings unchanged.
fn settings_error(state: &AppState, error: String) -> Response {
    let mut page = SettingsTemplate::new(state);
    page.error = Some(error);
    render(&page).with_status_code(400)
}

// ---------- Helpers ----------

/// Redirects to a magnet's card on the page a form came from. `back` must be
/// a path on this site; anything else goes to the home page.
fn back_to_card(back: Option<&str>, id: u64) -> Response {
    let path = back
        .filter(|b| b.starts_with('/') && !b.starts_with("//") && !b.contains(['\\', '#']))
        .unwrap_or("/");
    Response::redirect_303(format!("{path}#magnet-{id}"))
}

/// Locks the settings.
fn settings(state: &AppState) -> MutexGuard<'_, Settings> {
    lock(&state.settings)
}

/// Locks some shared state. A panic elsewhere while holding a lock can't
/// leave any of the state half-updated, so a poisoned lock is safe to keep using.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn render(template: &impl Template) -> Response {
    match template.render() {
        Ok(html) => Response::html(html),
        Err(e) => {
            log::error!("template error: {e}");
            Response::text("Internal server error").with_status_code(500)
        }
    }
}

/// Shows only the last four characters of a key, e.g. `••••••••abcd`.
/// Short keys are hidden entirely so the tail doesn't give most of it away.
fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let tail: String = if chars.len() >= 12 {
        chars[chars.len() - 4..].iter().collect()
    } else {
        String::new()
    };
    format!("{}{tail}", "•".repeat(8))
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_ago(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
    }
}

/// Whole percent, rounded down so 100% means really finished.
fn percent(done: u64, total: u64) -> u64 {
    if total == 0 {
        0
    } else {
        (done as u128 * 100 / total as u128) as u64
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(587_400_285), "560.2 MB");
    }

    #[test]
    fn ago() {
        assert_eq!(format_ago(-5), "just now");
        assert_eq!(format_ago(90), "1 min ago");
        assert_eq!(format_ago(7200), "2 h ago");
        assert_eq!(format_ago(200_000), "2 d ago");
    }

    #[test]
    fn masks_key() {
        assert_eq!(mask_key("abcdefgh1234"), "••••••••1234");
        assert_eq!(mask_key("abcd"), "••••••••");
    }

    #[test]
    fn pages() {
        let link = |s: &str| Some(s.to_string());
        let p = Pager::new(25, 1, 10);
        assert_eq!((p.first, p.last, p.range()), (1, 10, 0..10));
        assert_eq!((p.page, p.prev, p.next), (1, None, link("/?page=2")));
        let p = Pager::new(25, 2, 10);
        assert_eq!((p.prev, p.next), (link("/"), link("/?page=3")));
        let p = Pager::new(25, 3, 10);
        assert_eq!((p.first, p.last, p.range()), (21, 25, 20..25));
        assert_eq!((p.page, p.prev, p.next), (3, link("/?page=2"), None));
        let p = Pager::new(20, 2, 10);
        assert_eq!((p.next, p.first, p.last), (None, 11, 20));
        // Out-of-range pages show the nearest one.
        assert_eq!(Pager::new(25, 9, 10).page, 3);
        assert_eq!(Pager::new(25, 0, 10).page, 1);
        let p = Pager::new(0, 1, 10);
        assert_eq!((p.first, p.last, p.range()), (0, 0, 0..0));
        assert_eq!((p.page, p.prev, p.next), (1, None, None));
    }

    #[test]
    fn back_paths() {
        let location = |back| {
            let response = back_to_card(back, 7);
            let (_, value) = response.headers.iter().find(|(k, _)| k == "Location").unwrap();
            value.to_string()
        };
        assert_eq!(location(Some("/?page=2")), "/?page=2#magnet-7");
        assert_eq!(location(Some("/latest")), "/latest#magnet-7");
        assert_eq!(location(None), "/#magnet-7");
        assert_eq!(location(Some("//evil.example")), "/#magnet-7");
        assert_eq!(location(Some("https://evil.example")), "/#magnet-7");
    }

    #[test]
    fn percents() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(999, 1000), 99);
        assert_eq!(percent(1000, 1000), 100);
    }

    fn status(state: &str, done: u64, total: u64, speed: u64) -> aria2::Status {
        aria2::Status {
            status: state.to_string(),
            total_length: total,
            completed_length: done,
            download_speed: speed,
            error_message: None,
        }
    }

    #[test]
    fn download_labels() {
        let mut downloads = Downloads::new();
        downloads.insert("a".into(), Ok(status("active", 50, 200, 2048)));
        downloads.insert("b".into(), Ok(status("complete", 10, 10, 0)));
        downloads.insert("c".into(), Err("GID c is not found".into()));

        let label = |r: Result<&str, &str>| {
            download_label(&r.map(String::from).map_err(String::from), &downloads)
        };
        assert_eq!(label(Ok("a")), ("Downloading 25% · 2.0 KB/s".to_string(), "active"));
        assert_eq!(label(Ok("b")), ("Downloaded".to_string(), "ready"));
        assert_eq!(label(Ok("c")), ("No longer in aria2".to_string(), "muted"));
        assert_eq!(label(Ok("d")), ("Sent to aria2 (GID d)".to_string(), "muted"));
        assert_eq!(label(Err("bad")), ("aria2 refused it: bad".to_string(), "error"));
    }

    #[test]
    fn summarises_sent_magnet() {
        let sent = SentMagnet {
            at: 1000,
            classification: Classification {
                kind: Kind::Tv,
                detected_name: None,
                files: Vec::new(),
            },
            results: HashMap::from([
                ("x/1.mkv".to_string(), Ok("a".to_string())),
                ("x/2.mkv".to_string(), Ok("b".to_string())),
                ("x/3.mkv".to_string(), Err("nope".to_string())),
            ]),
        };
        let mut downloads = Downloads::new();
        downloads.insert("a".into(), Ok(status("active", 0, 1024, 1024)));
        downloads.insert("b".into(), Ok(status("complete", 1024, 1024, 0)));
        assert_eq!(
            sent_summary(&sent, &downloads, 1000 + 300),
            "Sent to aria2 5 min ago · 1 of 2 files downloaded · 1.0 KB of 2.0 KB (50%) · 1.0 KB/s · 1 file refused by aria2"
        );
        assert_eq!(
            sent_summary(&sent, &Downloads::new(), 1000),
            "Sent to aria2 just now · 1 file refused by aria2"
        );
    }
}
