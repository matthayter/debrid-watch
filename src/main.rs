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

    let state = Arc::new(AppState {
        alldebrid: alldebrid::Client::new(),
        settings: Mutex::new(Settings::default()),
    });

    log::info!("listening on http://{addr}");
    // rouille handles each request on its own thread from a pool; this call
    // blocks forever.
    rouille::start_server(addr, move |request| web::handle(request, &state));
}
