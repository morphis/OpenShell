// SPDX-FileCopyrightText: Copyright (c) 2026 Canonical Ltd.
// SPDX-License-Identifier: Apache-2.0

//! Thin async HTTP client for the LXD REST API over a Unix socket.

use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixStream;
use tracing::debug;

use crate::lxd::models::LxdResponse;

/// LXD REST API version prefix.
const API_PREFIX: &str = "/1.0";

/// Default timeout for individual LXD API calls.
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum allowed LXD instance name length.
const MAX_INSTANCE_NAME_LEN: usize = 63;

#[derive(Debug, thiserror::Error)]
pub enum LxdApiError {
    #[error("LXD API not found (404): {0}")]
    NotFound(String),
    #[error("LXD API conflict (409): {0}")]
    Conflict(String),
    #[error("LXD API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("connection error: {0}")]
    Connection(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("JSON error: {0}")]
    Json(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("operation failed: {0}")]
    OperationFailed(String),
}

/// Validate that an LXD instance name is safe for URL path interpolation.
///
/// LXD instance names must start and end with alphanumeric characters, contain
/// only alphanumerics and hyphens, and be at most 63 characters.
pub fn validate_instance_name(name: &str) -> Result<(), LxdApiError> {
    if name.is_empty() {
        return Err(LxdApiError::InvalidInput(
            "instance name must not be empty".to_string(),
        ));
    }
    if name.len() > MAX_INSTANCE_NAME_LEN {
        return Err(LxdApiError::InvalidInput(format!(
            "instance name exceeds maximum length of {MAX_INSTANCE_NAME_LEN} characters (got {})",
            name.len()
        )));
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() {
        return Err(LxdApiError::InvalidInput(format!(
            "instance name must start with an alphanumeric character: {name:?}"
        )));
    }
    if !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return Err(LxdApiError::InvalidInput(format!(
            "instance name must end with an alphanumeric character: {name:?}"
        )));
    }
    if !bytes
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(LxdApiError::InvalidInput(format!(
            "instance name may only contain alphanumerics and hyphens: {name:?}"
        )));
    }
    Ok(())
}

/// Derive a valid LXD instance name from a sandbox name.
///
/// Replaces non-alphanumeric characters with hyphens, collapses consecutive
/// hyphens, ensures the name starts and ends with alphanumeric, and truncates
/// to 63 characters.
pub fn sanitize_instance_name(name: &str) -> Result<String, LxdApiError> {
    // Replace invalid chars with hyphens.
    let replaced: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens.
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in replaced.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }

    // Ensure starts with alphanumeric.
    let result = if result.starts_with('-') {
        format!("os{result}")
    } else {
        result
    };

    // Truncate to 63 chars, then trim trailing hyphens.
    let result = if result.len() > MAX_INSTANCE_NAME_LEN {
        result[..MAX_INSTANCE_NAME_LEN].to_string()
    } else {
        result
    };
    let result = result.trim_end_matches('-').to_string();

    if result.is_empty() {
        return Err(LxdApiError::InvalidInput(
            "sandbox name produces empty instance name after sanitization".to_string(),
        ));
    }

    // Final validation.
    validate_instance_name(&result)?;
    Ok(result)
}

/// Async LXD REST API client communicating over a Unix socket.
#[derive(Debug, Clone)]
pub struct LxdClient {
    socket_path: PathBuf,
}

impl LxdClient {
    /// Create a new client targeting the given LXD Unix socket.
    #[must_use]
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Open a new HTTP/1.1 connection to the LXD socket.
    async fn connect(
        &self,
    ) -> Result<hyper::client::conn::http1::SendRequest<Full<Bytes>>, LxdApiError> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| LxdApiError::Connection(format!("{}: {e}", self.socket_path.display())))?;

        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| LxdApiError::Connection(e.to_string()))?;

        tokio::spawn(async move {
            if let Err(e) = conn.await {
                debug!(error = %e, "LXD API connection closed");
            }
        });

        Ok(sender)
    }

    /// Build an HTTP request targeting the LXD socket.
    fn build_request(
        method: hyper::Method,
        path: &str,
        body: Full<Bytes>,
        headers: Vec<(&'static str, String)>,
    ) -> Request<Full<Bytes>> {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("http://localhost{path}"))
            .header("Host", "localhost")
            .header("Accept", "application/json");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        builder.body(body).expect("valid request")
    }

    /// Send a request and return (status, body bytes).
    async fn send(
        &self,
        req: Request<Full<Bytes>>,
        timeout: Duration,
    ) -> Result<(hyper::StatusCode, Bytes), LxdApiError> {
        let mut sender = self.connect().await?;
        let response = tokio::time::timeout(timeout, sender.send_request(req))
            .await
            .map_err(|_| LxdApiError::Timeout(timeout))?
            .map_err(|e| LxdApiError::Connection(e.to_string()))?;
        let status = response.status();
        let bytes = tokio::time::timeout(timeout, response.into_body().collect())
            .await
            .map_err(|_| LxdApiError::Timeout(timeout))?
            .map_err(|e| LxdApiError::Connection(e.to_string()))?
            .to_bytes();
        Ok((status, bytes))
    }

    /// Perform an HTTP request and parse the LXD response envelope.
    ///
    /// `http_timeout` overrides the default `API_TIMEOUT`. Pass `None` to use
    /// the default. Long-lived requests such as operation waits must pass an
    /// explicit timeout that matches the LXD-side wait duration plus a buffer.
    pub(crate) async fn request_envelope(
        &self,
        method: hyper::Method,
        path: &str,
        body: Option<Bytes>,
        http_timeout: Option<Duration>,
    ) -> Result<LxdResponse, LxdApiError> {
        let (full_body, content_type) = body.map_or_else(
            || (Full::new(Bytes::new()), None::<String>),
            |b| (Full::new(b), Some("application/json".to_string())),
        );
        let mut headers = Vec::new();
        if let Some(ct) = content_type {
            headers.push(("Content-Type", ct));
        }
        let req = Self::build_request(method, path, full_body, headers);
        let (status, bytes) = self.send(req, http_timeout.unwrap_or(API_TIMEOUT)).await?;

        if !status.is_success() && status.as_u16() != 202 {
            return Err(parse_error(status.as_u16(), &bytes));
        }

        serde_json::from_slice::<LxdResponse>(&bytes)
            .map_err(|e| LxdApiError::Json(format!("{e}: {}", String::from_utf8_lossy(&bytes))))
    }

    /// GET a resource and return the typed sync metadata.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, LxdApiError> {
        let env = self
            .request_envelope(hyper::Method::GET, path, None, None)
            .await?;
        check_error(&env)?;
        env.parse_metadata::<T>()
            .map_err(|e| LxdApiError::Json(e.to_string()))?
            .ok_or_else(|| LxdApiError::Json("missing metadata in sync response".to_string()))
    }

    /// POST a JSON body and return the raw envelope (contains `operation` URL for async responses).
    pub async fn post<B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<LxdResponse, LxdApiError> {
        let payload = serde_json::to_vec(body).map_err(|e| LxdApiError::Json(e.to_string()))?;
        let env = self
            .request_envelope(hyper::Method::POST, path, Some(Bytes::from(payload)), None)
            .await?;
        check_error(&env)?;
        Ok(env)
    }

    /// PUT a JSON body and return the raw envelope.
    pub async fn put<B: Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<LxdResponse, LxdApiError> {
        let payload = serde_json::to_vec(body).map_err(|e| LxdApiError::Json(e.to_string()))?;
        let env = self
            .request_envelope(hyper::Method::PUT, path, Some(Bytes::from(payload)), None)
            .await?;
        check_error(&env)?;
        Ok(env)
    }

    /// DELETE a resource and return the raw envelope.
    pub async fn delete(&self, path: &str) -> Result<LxdResponse, LxdApiError> {
        let env = self
            .request_envelope(hyper::Method::DELETE, path, None, None)
            .await?;
        check_error(&env)?;
        Ok(env)
    }

    /// Push raw bytes as a file into a running instance.
    ///
    /// Sets `X-LXD-uid`, `X-LXD-gid`, `X-LXD-mode`, and `X-LXD-type` headers.
    /// The `mode` parameter is the octal permission bits (e.g. `0o755`).
    #[allow(clippy::too_many_arguments)]
    pub async fn push_file(
        &self,
        instance_name: &str,
        remote_path: &str,
        data: Vec<u8>,
        uid: u32,
        gid: u32,
        mode: u32,
        project: &str,
    ) -> Result<(), LxdApiError> {
        let encoded_path = url_encode(remote_path);
        let api_path = format!(
            "{API_PREFIX}/instances/{instance_name}/files?path={encoded_path}&project={project}"
        );

        let headers = vec![
            ("X-LXD-uid", uid.to_string()),
            ("X-LXD-gid", gid.to_string()),
            ("X-LXD-mode", format!("{mode:04o}")),
            ("X-LXD-type", "file".to_string()),
            ("Content-Type", "application/octet-stream".to_string()),
        ];

        let req = Self::build_request(
            hyper::Method::POST,
            &api_path,
            Full::new(Bytes::from(data)),
            headers,
        );

        let (status, bytes) = self.send(req, API_TIMEOUT).await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(parse_error(status.as_u16(), &bytes))
        }
    }

    /// Create a directory inside a running instance.
    ///
    /// Equivalent to `mkdir -p` — creates the directory and any missing parents.
    /// Uses `X-LXD-type: directory` and `X-LXD-mode` headers.
    pub async fn push_dir(
        &self,
        instance_name: &str,
        remote_path: &str,
        mode: u32,
        project: &str,
    ) -> Result<(), LxdApiError> {
        let encoded_path = url_encode(remote_path);
        let api_path = format!(
            "{API_PREFIX}/instances/{instance_name}/files?path={encoded_path}&project={project}"
        );

        let headers = vec![
            ("X-LXD-uid", "0".to_string()),
            ("X-LXD-gid", "0".to_string()),
            ("X-LXD-mode", format!("{mode:04o}")),
            ("X-LXD-type", "directory".to_string()),
        ];

        let req = Self::build_request(
            hyper::Method::POST,
            &api_path,
            Full::new(Bytes::new()),
            headers,
        );

        let (status, bytes) = self.send(req, API_TIMEOUT).await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(parse_error(status.as_u16(), &bytes))
        }
    }

    /// Check whether a file exists inside a running instance.
    ///
    /// Issues a HEAD request to the files API; returns `true` when the
    /// endpoint responds 200, `false` for 404. Other errors are propagated.
    pub async fn file_exists(
        &self,
        instance_name: &str,
        remote_path: &str,
        project: &str,
    ) -> Result<bool, LxdApiError> {
        let encoded_path = url_encode(remote_path);
        let api_path = format!(
            "{API_PREFIX}/instances/{instance_name}/files?path={encoded_path}&project={project}"
        );

        let req = Self::build_request(
            hyper::Method::GET,
            &api_path,
            Full::new(Bytes::new()),
            vec![],
        );

        let (status, _bytes) = self.send(req, API_TIMEOUT).await?;
        if status.is_success() {
            Ok(true)
        } else if status.as_u16() == 404 {
            Ok(false)
        } else {
            Err(LxdApiError::Api {
                status: status.as_u16(),
                message: format!("unexpected status checking file {remote_path}"),
            })
        }
    }

    /// Verify LXD is reachable by calling GET /1.0.
    pub async fn ping(&self) -> Result<(), LxdApiError> {
        let env = self
            .request_envelope(hyper::Method::GET, API_PREFIX, None, None)
            .await?;
        check_error(&env)
    }
}

/// Return an error if the envelope has `type == "error"`.
fn check_error(env: &LxdResponse) -> Result<(), LxdApiError> {
    if env.response_type == "error" {
        Err(LxdApiError::Api {
            status: env.error_code,
            message: env.error.clone(),
        })
    } else {
        Ok(())
    }
}

fn parse_error(status: u16, bytes: &Bytes) -> LxdApiError {
    let message = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).to_string());

    match status {
        404 => LxdApiError::NotFound(message),
        409 => LxdApiError::Conflict(message),
        _ => LxdApiError::Api { status, message },
    }
}

/// Minimal percent-encoding for URL query parameter values.
fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                String::from(b as char)
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_instance_name_accepts_valid_names() {
        assert!(validate_instance_name("my-vm").is_ok());
        assert!(validate_instance_name("openshell-abc123").is_ok());
        assert!(validate_instance_name("a").is_ok());
        assert!(validate_instance_name("vm1").is_ok());
    }

    #[test]
    fn validate_instance_name_rejects_invalid_names() {
        assert!(validate_instance_name("").is_err());
        assert!(validate_instance_name("-leading").is_err());
        assert!(validate_instance_name("trailing-").is_err());
        assert!(validate_instance_name("has_underscore").is_err());
        assert!(validate_instance_name("has.dot").is_err());
        assert!(validate_instance_name("has/slash").is_err());
        assert!(validate_instance_name("has space").is_err());
        let long = "a".repeat(64);
        assert!(validate_instance_name(&long).is_err());
    }

    #[test]
    fn sanitize_instance_name_handles_underscores() {
        let name = sanitize_instance_name("sandbox_abc_123").unwrap();
        assert_eq!(name, "sandbox-abc-123");
    }

    #[test]
    fn sanitize_instance_name_handles_leading_hyphen() {
        let name = sanitize_instance_name("-bad-start").unwrap();
        assert!(name.starts_with("os"));
    }

    #[test]
    fn sanitize_instance_name_truncates_long_names() {
        let long = "a".repeat(100);
        let name = sanitize_instance_name(&long).unwrap();
        assert!(name.len() <= 63);
    }

    #[test]
    fn sanitize_instance_name_collapses_consecutive_hyphens() {
        let name = sanitize_instance_name("a--b___c").unwrap();
        assert_eq!(name, "a-b-c");
    }

    #[test]
    fn url_encode_encodes_special_characters() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("/sandbox/path"), "%2Fsandbox%2Fpath");
        assert_eq!(url_encode("safe-_.~"), "safe-_.~");
    }
}
