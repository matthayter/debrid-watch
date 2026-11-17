mod alldebrid;
mod aria2;
mod media;
mod web;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use alldebrid::MagnetFile;
use media::Classification;

/// Everything the app knows lives here, in memory only; restarting the
/// server clears it.
pub struct AppState {
    pub alldebrid: alldebrid::Client,
    pub aria2: aria2::Client,
    pub settings: Mutex<Settings>,
    /// Unix time from which magnets count as new (normally the startup time).
    pub since: i64,
    /// File lists of ready magnets, by magnet ID. Fetched once per magnet.
    pub files: Mutex<HashMap<u64, CachedFiles>>,
    /// Show names / movie titles set by the user, by magnet ID.
    pub name_overrides: Mutex<HashMap<u64, String>>,
    /// Magnets sent (or being sent) to aria2, by magnet ID.
    pub sends: Mutex<HashMap<u64, SendState>>,
}

/// A ready magnet's name and file list, as fetched from AllDebrid.
pub struct CachedFiles {
    pub magnet_name: String,
    pub files: Vec<MagnetFile>,
}

#[derive(Clone)]
pub struct Settings {
    pub alldebrid_api_key: Option<String>,
    /// Username of the account the key belongs to, recorded when the key was saved.
    pub alldebrid_username: Option<String>,
    /// Root folder that tv/, movies/ and other/ go under, with no trailing `/`.
    pub dest_prefix: String,
    /// aria2 JSON-RPC URL, e.g. `http://host:6800/jsonrpc`.
    pub aria2_url: Option<String>,
    /// aria2's `--rpc-secret`, if it has one.
    pub aria2_secret: Option<String>,
}

impl Settings {
    pub fn aria2_endpoint(&self) -> Option<aria2::Endpoint<'_>> {
        Some(aria2::Endpoint {
            url: self.aria2_url.as_deref()?,
            secret: self.aria2_secret.as_deref(),
        })
    }
}

/// Where a magnet is in being sent to aria2.
pub enum SendState {
    /// Links are being unlocked and added to aria2 right now.
    Sending,
    /// Nothing was added to aria2 (a link couldn't be unlocked, or aria2
    /// couldn't be reached). It can be tried again.
    Failed { error: String },
    /// The included files were handed to aria2. Never sent again.
    Sent(SentMagnet),
}

pub struct SentMagnet {
    /// Unix time it was sent.
    pub at: i64,
    /// The destinations as they were when sent, so later changes to the
    /// prefix or name don't misreport where files went.
    pub classification: Classification,
    /// For each included file, by its path in the magnet: the aria2 GID, or
    /// why aria2 refused it.
    pub results: HashMap<String, Result<String, String>>,
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
    let aria2 = aria2::Client::new();
    let settings = initial_settings(&alldebrid, &aria2);

    let state = Arc::new(AppState {
        alldebrid,
        aria2,
        settings: Mutex::new(settings),
        since,
        files: Mutex::new(HashMap::new()),
        name_overrides: Mutex::new(HashMap::new()),
        sends: Mutex::new(HashMap::new()),
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

/// Checks an aria2 RPC URL: trims it and requires `http://` or `https://`.
pub fn normalize_aria2_url(url: &str) -> Result<String, &'static str> {
    let url = url.trim();
    if url.is_empty() {
        return Err("Enter the aria2 RPC URL.");
    }
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("The aria2 RPC URL must start with http:// or https://.");
    }
    Ok(url.to_string())
}

fn env_var(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Starting settings, taken from the environment. Anything set here can
/// still be changed on the Settings page while the server runs.
fn initial_settings(alldebrid: &alldebrid::Client, aria2: &aria2::Client) -> Settings {
    let mut settings = Settings {
        alldebrid_api_key: None,
        alldebrid_username: None,
        dest_prefix: env_var("DEBRID_WATCH_DEST_PREFIX")
            .and_then(|p| normalize_prefix(&p))
            .unwrap_or_else(|| DEFAULT_DEST_PREFIX.to_string()),
        aria2_url: None,
        aria2_secret: env_var("DEBRID_WATCH_ARIA2_SECRET"),
    };

    if let Some(url) = env_var("DEBRID_WATCH_ARIA2_URL") {
        match normalize_aria2_url(&url) {
            Ok(url) => settings.aria2_url = Some(url),
            Err(e) => log::warn!("ignoring DEBRID_WATCH_ARIA2_URL: {e}"),
        }
    }
    // As with the API key below, keep the URL even if aria2 can't be reached yet.
    if let Some(endpoint) = settings.aria2_endpoint() {
        match aria2.version(&endpoint) {
            Ok(v) => log::info!("using aria2 {v} at {}", endpoint.url),
            Err(e) => log::warn!("aria2 at {} could not be checked: {e}", endpoint.url),
        }
    }

    let Some(key) = env_var("ALLDEBRID_API_KEY") else {
        return settings;
    };

    // Keep the key even if the check fails, so a network blip at startup
    // doesn't drop it; the Home page will show any error from AllDebrid.
    match alldebrid.user(&key) {
        Ok(user) => {
            log::info!("using AllDebrid API key from ALLDEBRID_API_KEY (user {})", user.username);
            settings.alldebrid_username = Some(user.username);
        }
        Err(e) => log::warn!("AllDebrid API key from ALLDEBRID_API_KEY could not be verified: {e}"),
    }
    settings.alldebrid_api_key = Some(key);
    settings
}
