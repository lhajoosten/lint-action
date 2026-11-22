use super::app_report::REPORT_LIMIT;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::Read;
use std::time::Duration;
use ureq::http::{Request, Response, Uri};

#[derive(Debug)]
pub enum Error {
    Skip(String),
    Status { operation: String, status: u16 },
    Network,
    Metadata,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn skip(message: impl Into<String>) -> Self {
        Self::Skip(message.into())
    }

    pub fn status_message(operation: &str, status: u16) -> String {
        format!("GitHub rejected {operation} (HTTP {status}).")
    }
}

pub struct GitHub {
    agent: ureq::Agent,
    root: String,
    token: String,
}

impl GitHub {
    pub fn new(base_url: &str, repository: &str, token: String) -> Self {
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        Self {
            agent,
            root: format!("{base_url}/repos/{repository}"),
            token,
        }
    }

    fn request(
        &self,
        path: &str,
        method: &str,
        payload: Option<&Value>,
    ) -> Result<Response<ureq::Body>> {
        let request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.root))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("Content-Type", "application/json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .body(payload.map(Value::to_string).unwrap_or_default())
            .map_err(|_| Error::Metadata)?;
        self.agent.run(request).map_err(|_| Error::Network)
    }

    pub fn json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(path, "GET", None)
    }

    pub fn send<T: DeserializeOwned>(
        &self,
        path: &str,
        method: &str,
        payload: Option<&Value>,
    ) -> Result<T> {
        let mut response = self.request(path, method, payload)?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Status {
                operation: format!("{method} {path}"),
                status: status.as_u16(),
            });
        }
        let bytes = read(&mut response, 4 * 1024 * 1024)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Metadata)
    }

    pub fn pages(&self, path: &str, key: &str) -> Result<Vec<Value>> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let mut items = Vec::new();
        for page in 1..=20 {
            let value: Value = self.json(&format!("{path}{separator}per_page=100&page={page}"))?;
            let entries = value[key].as_array().ok_or(Error::Metadata)?;
            let total = value["total_count"].as_u64().ok_or(Error::Metadata)?;
            items.extend(entries.iter().cloned());
            if entries.len() < 100 || page * 100 >= total {
                return Ok(items);
            }
        }
        Err(Error::Metadata)
    }

    pub fn artifact(&self, id: u64) -> Result<Vec<u8>> {
        let response = self.request(&format!("/actions/artifacts/{id}/zip"), "GET", None)?;
        let location = response
            .headers()
            .get("Location")
            .and_then(|value| value.to_str().ok())
            .and_then(|location| location.parse::<Uri>().ok())
            .filter(|uri| uri.scheme_str() == Some("https") && uri.host().is_some());
        let location = location
            .filter(|_| response.status() == 302)
            .ok_or_else(|| Error::Status {
                operation: "report download".into(),
                status: response.status().as_u16(),
            })?;
        // The signed URL gets no GitHub credentials. Redirects are disabled on both requests.
        let request = Request::get(location)
            .body(())
            .map_err(|_| Error::Metadata)?;
        let mut download = self.agent.run(request).map_err(|_| Error::Network)?;
        if !download.status().is_success() {
            return Err(Error::Metadata);
        }
        read(&mut download, REPORT_LIMIT)
    }
}

fn read(response: &mut Response<ureq::Body>, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Network)?;
    if bytes.len() > limit {
        return Err(Error::Metadata);
    }
    Ok(bytes)
}
