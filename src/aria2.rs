//! Minimal client for aria2's JSON-RPC interface over HTTP
//! (https://aria2.github.io/manual/en/html/aria2c.html#rpc-interface).

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub struct Client {
    agent: ureq::Agent,
}

/// Where aria2 is and how to authenticate with it.
pub struct Endpoint<'a> {
    /// RPC URL, e.g. `http://host:6800/jsonrpc`.
    pub url: &'a str,
    /// The `--rpc-secret`, if aria2 has one.
    pub secret: Option<&'a str>,
}

/// One file to add to aria2.
pub struct Download<'a> {
    /// Direct download URL.
    pub url: &'a str,
    /// Folder to save into, on the aria2 machine.
    pub dir: &'a str,
    /// File name to save as, inside `dir`.
    pub out: &'a str,
}

/// The parts of `aria2.tellStatus` that are shown on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// `active`, `waiting`, `paused`, `error`, `complete` or `removed`.
    pub status: String,
    pub total_length: u64,
    pub completed_length: u64,
    /// Bytes per second.
    pub download_speed: u64,
    pub error_message: Option<String>,
}

/// Fields asked for in `aria2.tellStatus`, so aria2 doesn't send the rest.
const STATUS_KEYS: [&str; 5] = [
    "status",
    "totalLength",
    "completedLength",
    "downloadSpeed",
    "errorMessage",
];

/// Timeout for adding downloads and getting their status.
const TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for single calls such as the version check, which the Settings
/// page makes each time it loads.
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

impl Client {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .user_agent(concat!("debrid-watch/", env!("CARGO_PKG_VERSION")))
            .timeout_global(Some(TIMEOUT))
            // aria2 reports RPC errors in the JSON body, with a non-2xx
            // status, so read the body regardless of the status code.
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
        }
    }

    /// Returns aria2's version. Doubles as a connection and secret check.
    pub fn version(&self, endpoint: &Endpoint) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Version {
            version: String,
        }
        let v: Version = self.call(endpoint, "aria2.getVersion", vec![])?;
        Ok(v.version)
    }

    /// Adds several downloads in one request (`system.multicall`). Each gets
    /// its own result: the new download's GID, or why aria2 refused it.
    pub fn add_uris(
        &self,
        endpoint: &Endpoint,
        downloads: &[Download],
    ) -> Result<Vec<Result<String, RpcError>>, Error> {
        let calls = downloads
            .iter()
            .map(|d| {
                let params = vec![json!([d.url]), json!({"dir": d.dir, "out": d.out})];
                ("aria2.addUri", params)
            })
            .collect();
        self.multicall(endpoint, calls)
    }

    /// Fetches the status of several downloads in one request. A download
    /// aria2 no longer knows about (for example after it restarted) gets an error.
    pub fn statuses(
        &self,
        endpoint: &Endpoint,
        gids: &[&str],
    ) -> Result<Vec<Result<Status, RpcError>>, Error> {
        let calls = gids
            .iter()
            .map(|gid| ("aria2.tellStatus", vec![json!(gid), json!(STATUS_KEYS)]))
            .collect();
        let raw: Vec<Result<RawStatus, RpcError>> = self.multicall(endpoint, calls)?;
        Ok(raw.into_iter().map(|r| r.map(RawStatus::into_status)).collect())
    }

    fn call<T: DeserializeOwned>(
        &self,
        endpoint: &Endpoint,
        method: &str,
        params: Vec<Value>,
    ) -> Result<T, Error> {
        let body = request_body(method, with_token(endpoint.secret, params));
        let result = self.post(endpoint, &body, CHECK_TIMEOUT)?;
        serde_json::from_value(result).map_err(Error::Parse)
    }

    /// Runs several calls in one request. Each call needs the secret itself;
    /// `system.multicall` doesn't take one.
    fn multicall<T: DeserializeOwned>(
        &self,
        endpoint: &Endpoint,
        calls: Vec<(&str, Vec<Value>)>,
    ) -> Result<Vec<Result<T, RpcError>>, Error> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let calls: Vec<Value> = calls
            .into_iter()
            .map(|(method, params)| {
                json!({"methodName": method, "params": with_token(endpoint.secret, params)})
            })
            .collect();
        let body = request_body("system.multicall", vec![Value::Array(calls)]);
        parse_multicall(self.post(endpoint, &body, TIMEOUT)?)
    }

    fn post(&self, endpoint: &Endpoint, body: &Value, timeout: Duration) -> Result<Value, Error> {
        let mut response = self
            .agent
            .post(endpoint.url)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .send_json(body)?;
        let reply: Reply = response.body_mut().read_json()?;
        reply.into_result()
    }
}

fn request_body(method: &str, params: Vec<Value>) -> Value {
    json!({"jsonrpc": "2.0", "id": "debrid-watch", "method": method, "params": params})
}

/// Puts `token:<secret>` first in the parameters, as aria2 expects.
fn with_token(secret: Option<&str>, mut params: Vec<Value>) -> Vec<Value> {
    if let Some(secret) = secret {
        params.insert(0, Value::String(format!("token:{secret}")));
    }
    params
}

/// `system.multicall` returns one item per call: a one-element array
/// holding the result, or an error object.
fn parse_multicall<T: DeserializeOwned>(result: Value) -> Result<Vec<Result<T, RpcError>>, Error> {
    let Value::Array(items) = result else {
        return Err(Error::Unexpected("system.multicall did not return a list".to_string()));
    };
    items
        .into_iter()
        .map(|item| match item {
            Value::Array(mut one) if one.len() == 1 => {
                serde_json::from_value(one.remove(0)).map(Ok).map_err(Error::Parse)
            }
            other => serde_json::from_value::<RpcError>(other)
                .map(Err)
                .map_err(Error::Parse),
        })
        .collect()
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

impl Reply {
    fn into_result(self) -> Result<Value, Error> {
        match (self.result, self.error) {
            (_, Some(error)) => Err(Error::Rpc(error)),
            (Some(result), None) => Ok(result),
            (None, None) => Err(Error::Unexpected("reply had no result".to_string())),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    #[serde(alias = "faultCode")]
    pub code: i64,
    #[serde(alias = "faultString")]
    pub message: String,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug)]
pub enum Error {
    Http(ureq::Error),
    Rpc(RpcError),
    Parse(serde_json::Error),
    Unexpected(String),
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Error::Http(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "could not reach aria2: {e}"),
            Error::Rpc(e) => write!(f, "aria2 error: {}", e.message),
            Error::Parse(e) => write!(f, "unexpected reply from aria2: {e}"),
            Error::Unexpected(e) => write!(f, "unexpected reply from aria2: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// `tellStatus` as aria2 sends it: numbers are strings.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStatus {
    status: String,
    #[serde(default)]
    total_length: String,
    #[serde(default)]
    completed_length: String,
    #[serde(default)]
    download_speed: String,
    #[serde(default)]
    error_message: Option<String>,
}

impl RawStatus {
    fn into_status(self) -> Status {
        let num = |s: &str| s.parse().unwrap_or(0);
        Status {
            total_length: num(&self.total_length),
            completed_length: num(&self.completed_length),
            download_speed: num(&self.download_speed),
            error_message: self.error_message.filter(|m| !m.is_empty()),
            status: self.status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_token_first() {
        let params = with_token(Some("s3cret"), vec![json!(["http://x/a"])]);
        assert_eq!(params, vec![json!("token:s3cret"), json!(["http://x/a"])]);
        assert_eq!(with_token(None, vec![json!(1)]), vec![json!(1)]);
    }

    #[test]
    fn parses_error_reply() {
        let reply: Reply = serde_json::from_str(
            r#"{"id":"debrid-watch","jsonrpc":"2.0","error":{"code":1,"message":"Unauthorized"}}"#,
        )
        .unwrap();
        match reply.into_result() {
            Err(Error::Rpc(e)) => assert_eq!(e.message, "Unauthorized"),
            other => panic!("expected an RPC error, got {other:?}"),
        }
    }

    #[test]
    fn parses_multicall_with_mixed_results() {
        let reply: Reply = serde_json::from_str(
            r#"{"id":"debrid-watch","jsonrpc":"2.0","result":[
                ["2089b05ecca3d829"],
                {"code":1,"message":"No URI to download."}
            ]}"#,
        )
        .unwrap();
        let results: Vec<Result<String, RpcError>> =
            parse_multicall(reply.into_result().unwrap()).unwrap();
        assert_eq!(results[0].as_ref().unwrap(), "2089b05ecca3d829");
        assert_eq!(results[1].as_ref().unwrap_err().message, "No URI to download.");
    }

    #[test]
    fn parses_status() {
        let raw: RawStatus = serde_json::from_str(
            r#"{"status":"active","totalLength":"34896138","completedLength":"34896138",
                "downloadSpeed":"1024","errorMessage":""}"#,
        )
        .unwrap();
        assert_eq!(
            raw.into_status(),
            Status {
                status: "active".to_string(),
                total_length: 34896138,
                completed_length: 34896138,
                download_speed: 1024,
                error_message: None,
            }
        );
    }
}
