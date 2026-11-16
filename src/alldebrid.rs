//! Minimal client for the AllDebrid HTTP API (https://docs.alldebrid.com/).

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

const BASE_URL: &str = "https://api.alldebrid.com";

pub struct Client {
    agent: ureq::Agent,
}

impl Client {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .user_agent(concat!("debrid-watch/", env!("CARGO_PKG_VERSION")))
            .timeout_global(Some(Duration::from_secs(15)))
            // AllDebrid reports failures in the JSON body, sometimes with a
            // non-2xx status, so read the body regardless of the status code.
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
        }
    }

    /// Fetches the account behind `api_key`. Doubles as an API key check.
    pub fn user(&self, api_key: &str) -> Result<User, Error> {
        let response = self
            .agent
            .get(format!("{BASE_URL}/v4/user"))
            .header("Authorization", format!("Bearer {api_key}"))
            .call()?;
        let data: UserData = parse(response)?;
        Ok(data.user)
    }

    /// Lists all magnets on the account, in whatever order AllDebrid returns them.
    pub fn magnets(&self, api_key: &str) -> Result<Vec<Magnet>, Error> {
        let response = self
            .agent
            .post(format!("{BASE_URL}/v4.1/magnet/status"))
            .header("Authorization", format!("Bearer {api_key}"))
            .send_empty()?;
        let data: MagnetsData = parse(response)?;
        Ok(data.magnets.into_vec())
    }
}

fn parse<T: DeserializeOwned>(mut response: ureq::http::Response<ureq::Body>) -> Result<T, Error> {
    let envelope: Envelope<T> = response.body_mut().read_json()?;
    match envelope {
        Envelope::Success { data } => Ok(data),
        Envelope::Error { error } => Err(Error::Api(error)),
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum Envelope<T> {
    Success { data: T },
    Error { error: ApiError },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub enum Error {
    Http(ureq::Error),
    Api(ApiError),
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Error::Http(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "could not reach AllDebrid: {e}"),
            Error::Api(e) => write!(f, "{} ({})", e.message, e.code),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Deserialize)]
struct UserData {
    user: User,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub username: String,
    #[serde(default)]
    pub is_premium: bool,
}

#[derive(Deserialize)]
struct MagnetsData {
    magnets: MagnetList,
}

/// The docs show `magnets` as an array, but some AllDebrid responses return
/// an object keyed by magnet ID instead, so accept both.
#[derive(Deserialize)]
#[serde(untagged)]
enum MagnetList {
    List(Vec<Magnet>),
    Map(BTreeMap<String, Magnet>),
}

impl MagnetList {
    fn into_vec(self) -> Vec<Magnet> {
        match self {
            MagnetList::List(v) => v,
            MagnetList::Map(m) => m.into_values().collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Magnet {
    pub id: u64,
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    pub status: String,
    pub status_code: u32,
    /// Unix timestamp of when the magnet was added to AllDebrid.
    #[serde(default)]
    pub upload_date: i64,
}

impl Magnet {
    pub fn is_ready(&self) -> bool {
        self.status_code == 4
    }

    pub fn is_error(&self) -> bool {
        self.status_code >= 5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_magnet_list() {
        let body = r#"{
            "status": "success",
            "data": {
                "magnets": [{
                    "id": 123456,
                    "filename": "ubuntu-16.04.2-live-server-amd64.iso",
                    "size": 587400285,
                    "status": "Downloading",
                    "statusCode": 1,
                    "downloaded": 255400192,
                    "uploaded": 0,
                    "seeders": 7,
                    "downloadSpeed": 18874368,
                    "uploadSpeed": 0,
                    "uploadDate": 1557133868,
                    "completionDate": 0
                }]
            }
        }"#;
        let env: Envelope<MagnetsData> = serde_json::from_str(body).unwrap();
        let Envelope::Success { data } = env else {
            panic!("expected success")
        };
        let magnets = data.magnets.into_vec();
        assert_eq!(magnets.len(), 1);
        assert_eq!(magnets[0].id, 123456);
        assert_eq!(magnets[0].status_code, 1);
        assert_eq!(magnets[0].upload_date, 1557133868);
    }

    #[test]
    fn parses_magnet_map() {
        let body = r#"{"status":"success","data":{"magnets":{
            "1": {"id":1,"filename":"a","size":1,"status":"Ready","statusCode":4,"uploadDate":10}
        }}}"#;
        let env: Envelope<MagnetsData> = serde_json::from_str(body).unwrap();
        let Envelope::Success { data } = env else {
            panic!("expected success")
        };
        assert_eq!(data.magnets.into_vec()[0].filename, "a");
    }

    #[test]
    fn parses_error() {
        let body = r#"{"status":"error","error":{"code":"AUTH_BAD_APIKEY","message":"The auth apikey is invalid"}}"#;
        let env: Envelope<UserData> = serde_json::from_str(body).unwrap();
        let Envelope::Error { error } = env else {
            panic!("expected error")
        };
        assert_eq!(error.code, "AUTH_BAD_APIKEY");
    }
}
