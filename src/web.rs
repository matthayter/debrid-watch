use std::collections::{BTreeSet, HashMap};
use std::sync::MutexGuard;

use askama::Template;
use rouille::{Request, Response, post_input, router};

use crate::alldebrid::{Magnet, MagnetFile};
use crate::media::{self, Decision, Kind};
use crate::{AppState, Settings, normalize_prefix, unix_now};

/// Most magnet IDs sent in one `magnet/files` request.
const FILES_BATCH_SIZE: usize = 50;

/// Entry point for every HTTP request.
pub fn handle(request: &Request, state: &AppState) -> Response {
    router!(request,
        (GET) ["/"] => { home(state) },
        (GET) ["/settings"] => { settings_page(request, state) },
        (POST) ["/settings"] => { save_settings(request, state) },
        (POST) ["/magnets/{id}/name", id: u64] => { set_name(request, state, id) },
        _ => Response::empty_404()
    )
}

// ---------- Home ----------

#[derive(Template)]
#[template(path = "home.html")]
struct HomeTemplate {
    has_api_key: bool,
    error: Option<String>,
    /// How long ago "new" starts, e.g. "5 min ago".
    since_ago: String,
    magnets: Vec<MagnetCard>,
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
    /// The editable show name or movie title, for TV and movies.
    media_name: Option<MediaName>,
    /// Distinct destination folders of the included files.
    destinations: Vec<String>,
    files: Vec<FileRow>,
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
}

fn home(state: &AppState) -> Response {
    // Copy what's needed so the lock isn't held during AllDebrid requests.
    let (api_key, prefix) = {
        let s = settings(state);
        (s.alldebrid_api_key.clone(), s.dest_prefix.clone())
    };
    let now = unix_now();

    let mut page = HomeTemplate {
        has_api_key: api_key.is_some(),
        error: None,
        since_ago: format_ago(now - state.since),
        magnets: Vec::new(),
    };
    let Some(key) = api_key else {
        return render(&page);
    };

    let mut magnets = match state.alldebrid.magnets(&key) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("failed to list magnets: {e}");
            page.error = Some(e.to_string());
            return render(&page);
        }
    };
    magnets.retain(|m| m.upload_date >= state.since);
    magnets.sort_by(|a, b| b.upload_date.cmp(&a.upload_date));

    let file_errors = fetch_missing_files(state, &key, &magnets);
    let cache = state.files.lock().unwrap_or_else(|e| e.into_inner());
    let overrides = state.name_overrides.lock().unwrap_or_else(|e| e.into_inner());
    page.magnets = magnets
        .iter()
        .map(|m| {
            let files = cache.get(&m.id).map(Vec::as_slice);
            let error = file_errors.get(&m.id).cloned();
            let name_override = overrides.get(&m.id).map(String::as_str);
            magnet_card(m, files, error, name_override, &prefix, now)
        })
        .collect();
    drop(overrides);
    drop(cache);

    render(&page)
}

/// Fetches file lists for ready magnets that aren't cached yet, and caches
/// them. Returns an error message for each magnet whose list couldn't be fetched.
fn fetch_missing_files(state: &AppState, key: &str, magnets: &[Magnet]) -> HashMap<u64, String> {
    let missing: Vec<u64> = {
        let cache = state.files.lock().unwrap_or_else(|e| e.into_inner());
        magnets
            .iter()
            .filter(|m| m.is_ready() && !cache.contains_key(&m.id))
            .map(|m| m.id)
            .collect()
    };

    let mut errors = HashMap::new();
    for ids in missing.chunks(FILES_BATCH_SIZE) {
        log::info!("fetching file lists for {} magnet(s)", ids.len());
        match state.alldebrid.magnet_files(key, ids) {
            Ok(results) => {
                let mut cache = state.files.lock().unwrap_or_else(|e| e.into_inner());
                for (id, result) in results {
                    match result {
                        Ok(files) => {
                            cache.insert(id, files);
                        }
                        Err(e) => {
                            errors.insert(id, format!("{} ({})", e.message, e.code));
                        }
                    }
                }
                for id in ids {
                    if !cache.contains_key(id) && !errors.contains_key(id) {
                        errors.insert(*id, "AllDebrid returned no file list".to_string());
                    }
                }
            }
            Err(e) => {
                log::warn!("failed to fetch file lists: {e}");
                for id in ids {
                    errors.insert(*id, e.to_string());
                }
            }
        }
    }
    errors
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
    let mut card = MagnetCard {
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
    };

    if let (true, Some(files)) = (m.is_ready(), files) {
        let classification = media::classify(&m.filename, files, prefix, name_override);
        card.kind = Some(classification.kind.label());
        card.media_name = classification.detected_name.map(|detected| MediaName {
            label: match classification.kind {
                Kind::Movie => "Movie title",
                _ => "Show name",
            },
            current: name_override.unwrap_or(&detected).to_string(),
            overridden: name_override.is_some_and(|n| n != detected),
            detected,
        });
        let top_folder = format!("{}/", m.filename);
        let mut destinations = BTreeSet::new();
        card.files = classification
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
        card.destinations = destinations.into_iter().collect();
    }
    card
}

/// Sets or clears the show name / movie title used for one magnet.
fn set_name(request: &Request, state: &AppState, id: u64) -> Response {
    let form = match post_input!(request, {
        name: Option<String>,
        action: Option<String>,
    }) {
        Ok(form) => form,
        Err(_) => return Response::text("Bad form data").with_status_code(400),
    };

    let mut overrides = state.name_overrides.lock().unwrap_or_else(|e| e.into_inner());
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
    Response::redirect_303(format!("/#magnet-{id}"))
}

// ---------- Settings ----------

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    masked_key: Option<String>,
    username: Option<String>,
    dest_prefix: String,
    saved: bool,
    error: Option<String>,
}

impl SettingsTemplate {
    fn new(settings: &Settings) -> Self {
        Self {
            masked_key: settings.alldebrid_api_key.as_deref().map(mask_key),
            username: settings.alldebrid_username.clone(),
            dest_prefix: settings.dest_prefix.clone(),
            saved: false,
            error: None,
        }
    }
}

fn settings_page(request: &Request, state: &AppState) -> Response {
    let mut page = SettingsTemplate::new(&settings(state));
    page.saved = request.get_param("saved").is_some();
    render(&page)
}

fn save_settings(request: &Request, state: &AppState) -> Response {
    let form = match post_input!(request, {
        alldebrid_api_key: Option<String>,
        dest_prefix: Option<String>,
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
    let mut page = SettingsTemplate::new(&settings(state));
    page.error = Some(error);
    render(&page).with_status_code(400)
}

// ---------- Helpers ----------

/// Locks the settings. A panic elsewhere while holding the lock can't leave
/// `Settings` half-updated, so a poisoned lock is safe to keep using.
fn settings(state: &AppState) -> MutexGuard<'_, Settings> {
    state.settings.lock().unwrap_or_else(|e| e.into_inner())
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
}
