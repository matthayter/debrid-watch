mod alldebrid;
mod media;
mod web;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use alldebrid::MagnetFile;

/// Everything the app knows lives here, in memory only; restarting the
/// server clears it.
pub struct AppState {
    pub alldebrid: alldebrid::Client,
    pub settings: Mutex<Settings>,
    /// Unix time from which magnets count as new (normally the startup time).
    pub since: i64,
    /// File lists of ready magnets, by magnet ID. Fetched once per magnet.
    pub files: Mutex<HashMap<u64, Vec<MagnetFile>>>,
    /// Show names / movie titles set by the user, by magnet ID.
    pub name_overrides: Mutex<HashMap<u64, String>>,
}

#[derive(Clone)]
pub struct Settings {
    pub alldebrid_api_key: Option<String>,
    /// Username of the account the key belongs to, recorded when the key was saved.
    pub alldebrid_username: Option<String>,
    /// Root folder that tv/, movies/ and other/ go under, with no trailing `/`.
    pub dest_prefix: String,
}

const DEFAULT_DEST_PREFIX: &str = "/downloads";

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let addr = std::env::var("DEBRID_WATCH_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let since = match std::env::var("DEBRID_WATCH_SINCE") {
        Ok(v) => v
            .trim()
            .parse()
            .expect("DEBRID_WATCH_SINCE must be a unix timestamp in seconds"),
        Err(_) => unix_now(),
    };

    let alldebrid = alldebrid::Client::new();
    let settings = initial_settings(&alldebrid);

    let state = Arc::new(AppState {
        alldebrid,
        settings: Mutex::new(settings),
        since,
        files: Mutex::new(HashMap::new()),
        name_overrides: Mutex::new(HashMap::new()),
    });

    log::info!("showing magnets added since unix time {since}");
    log::info!("listening on http://{addr}");
    // rouille handles each request on its own thread from a pool; this call
    // blocks forever.
    rouille::start_server(addr, move |request| web::handle(request, &state));
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Normalises a destination prefix: trims spaces and trailing slashes.
/// Returns `None` if nothing is left.
pub fn normalize_prefix(prefix: &str) -> Option<String> {
    let p = prefix.trim();
    if p.is_empty() {
        return None;
    }
    Some(p.trim_end_matches('/').to_string())
}

/// Starting settings, taken from the environment. Anything set here can
/// still be changed on the Settings page while the server runs.
fn initial_settings(alldebrid: &alldebrid::Client) -> Settings {
    let mut settings = Settings {
        alldebrid_api_key: None,
        alldebrid_username: None,
        dest_prefix: std::env::var("DEBRID_WATCH_DEST_PREFIX")
            .ok()
            .and_then(|p| normalize_prefix(&p))
            .unwrap_or_else(|| DEFAULT_DEST_PREFIX.to_string()),
    };

    let key = std::env::var("ALLDEBRID_API_KEY").unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        return settings;
    }

    // Keep the key even if the check fails, so a network blip at startup
    // doesn't drop it; the Home page will show any error from AllDebrid.
    match alldebrid.user(key) {
        Ok(user) => {
            log::info!("using AllDebrid API key from ALLDEBRID_API_KEY (user {})", user.username);
            settings.alldebrid_username = Some(user.username);
        }
        Err(e) => log::warn!("AllDebrid API key from ALLDEBRID_API_KEY could not be verified: {e}"),
    }
    settings.alldebrid_api_key = Some(key.to_string());
    settings
}
