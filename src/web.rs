use std::sync::MutexGuard;
use std::time::{SystemTime, UNIX_EPOCH};

use askama::Template;
use rouille::{Request, Response, post_input, router};

use crate::alldebrid::Magnet;
use crate::{AppState, Settings};

const HOME_MAGNET_LIMIT: usize = 10;

/// Entry point for every HTTP request.
pub fn handle(request: &Request, state: &AppState) -> Response {
    router!(request,
        (GET) ["/"] => { home(state) },
        (GET) ["/settings"] => { settings_page(request, state) },
        (POST) ["/settings"] => { save_settings(request, state) },
        _ => Response::empty_404()
    )
}

// ---------- Home ----------

#[derive(Template)]
#[template(path = "home.html")]
struct HomeTemplate {
    has_api_key: bool,
    error: Option<String>,
    magnets: Vec<MagnetRow>,
}

struct MagnetRow {
    filename: String,
    size: String,
    status: String,
    status_class: &'static str,
    added: String,
}

impl MagnetRow {
    fn from_magnet(m: &Magnet, now: i64) -> Self {
        let status_class = if m.is_ready() {
            "ready"
        } else if m.is_error() {
            "error"
        } else {
            "active"
        };
        Self {
            filename: m.filename.clone(),
            size: format_size(m.size),
            status: m.status.clone(),
            status_class,
            added: format_ago(now - m.upload_date),
        }
    }
}

fn home(state: &AppState) -> Response {
    // Copy the key out so the lock isn't held during the AllDebrid request.
    let api_key = settings(state).alldebrid_api_key.clone();

    let mut page = HomeTemplate {
        has_api_key: api_key.is_some(),
        error: None,
        magnets: Vec::new(),
    };

    if let Some(key) = api_key {
        match state.alldebrid.magnets(&key) {
            Ok(mut magnets) => {
                magnets.sort_by(|a, b| b.upload_date.cmp(&a.upload_date));
                let now = unix_now();
                page.magnets = magnets
                    .iter()
                    .take(HOME_MAGNET_LIMIT)
                    .map(|m| MagnetRow::from_magnet(m, now))
                    .collect();
            }
            Err(e) => {
                log::warn!("failed to list magnets: {e}");
                page.error = Some(e.to_string());
            }
        }
    }

    render(&page)
}

// ---------- Settings ----------

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    masked_key: Option<String>,
    username: Option<String>,
    saved: bool,
    error: Option<String>,
}

impl SettingsTemplate {
    fn new(settings: &Settings) -> Self {
        Self {
            masked_key: settings.alldebrid_api_key.as_deref().map(mask_key),
            username: settings.alldebrid_username.clone(),
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
        action: Option<String>,
    }) {
        Ok(form) => form,
        Err(_) => return Response::text("Bad form data").with_status_code(400),
    };

    if form.action.as_deref() == Some("remove") {
        let mut s = settings(state);
        s.alldebrid_api_key = None;
        s.alldebrid_username = None;
        log::info!("AllDebrid API key removed");
        return Response::redirect_303("/settings?saved");
    }

    let key = form.alldebrid_api_key.unwrap_or_default();
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

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
