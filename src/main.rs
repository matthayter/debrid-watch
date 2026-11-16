mod alldebrid;
mod web;

use std::sync::{Arc, Mutex};

/// Everything the app knows lives here, in memory only; restarting the
/// server clears it.
pub struct AppState {
    pub alldebrid: alldebrid::Client,
    pub settings: Mutex<Settings>,
}

#[derive(Default, Clone)]
pub struct Settings {
    pub alldebrid_api_key: Option<String>,
    /// Username of the account the key belongs to, recorded when the key was saved.
    pub alldebrid_username: Option<String>,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let addr = std::env::var("DEBRID_WATCH_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());

    let alldebrid = alldebrid::Client::new();
    let settings = initial_settings(&alldebrid);

    let state = Arc::new(AppState {
        alldebrid,
        settings: Mutex::new(settings),
    });

    log::info!("listening on http://{addr}");
    // rouille handles each request on its own thread from a pool; this call
    // blocks forever.
    rouille::start_server(addr, move |request| web::handle(request, &state));
}

/// Starting settings, taken from the environment. Anything set here can
/// still be changed on the Settings page while the server runs.
fn initial_settings(alldebrid: &alldebrid::Client) -> Settings {
    let mut settings = Settings::default();

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
