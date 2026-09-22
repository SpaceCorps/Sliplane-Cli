//! HTTP to the Sliplane API, and the translation from HTTP status to [`ErrorCode`].
//!
//! One blocking agent per process: a CLI makes a handful of requests, so an async runtime
//! would cost more in startup than it could save. Connections are pooled by the agent, so the
//! commands that fan out (`services list` across projects) reuse TLS sessions.

use std::time::Duration;

use serde_json::Value;
use ureq::Agent;
use ureq::http::Response;

use crate::error::{Error, ErrorCode, Result};

const DEFAULT_BASE: &str = "https://ctrl.sliplane.io/v0/";
const MAX_BODY: u64 = 512 * 1024 * 1024;

#[derive(Clone)]
pub struct Client {
    agent: Agent,
    base: String,
    auth: String,
    org_id: Option<String>,
}

enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Client {
    pub fn new(api_key: &str, org_id: Option<&str>) -> Client {
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(100)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .user_agent(concat!("sliplane-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();

        // For pointing the CLI at a mock server in tests. It changes where requests go, never
        // which key they carry - the key still comes only from a named account.
        let mut base = std::env::var("SLIPLANE_API_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE.to_string());
        if !base.ends_with('/') {
            base.push('/');
        }

        Client {
            agent,
            base,
            auth: format!("Bearer {api_key}"),
            org_id: org_id.filter(|s| !s.is_empty()).map(str::to_string),
        }
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.send(Method::Get, path, None)
    }

    /// POST with no body at all - the pause/unpause style endpoints.
    pub fn post_empty(&self, path: &str) -> Result<Value> {
        self.send(Method::Post, path, None)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(Method::Post, path, Some(body))
    }

    pub fn put(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(Method::Put, path, Some(body))
    }

    pub fn patch(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(Method::Patch, path, Some(body))
    }

    /// DELETE, returning whatever body came back (`{}` for a 204).
    pub fn delete(&self, path: &str) -> Result<Value> {
        self.send(Method::Delete, path, None)
    }

    fn send(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{}", self.base, path);

        macro_rules! headers {
            ($req:expr) => {{
                let mut r = $req.header("Authorization", &self.auth).header("Accept", "application/json");
                if let Some(org) = &self.org_id {
                    r = r.header("X-Organization-ID", org);
                }
                r
            }};
        }

        let result = match (method, body) {
            (Method::Get, _) => headers!(self.agent.get(&url)).call(),
            (Method::Delete, _) => headers!(self.agent.delete(&url)).call(),
            (m, Some(b)) => {
                let json = serde_json::to_vec(b).expect("a Value always serializes");
                let req = match m {
                    Method::Post => self.agent.post(&url),
                    Method::Put => self.agent.put(&url),
                    _ => self.agent.patch(&url),
                };
                headers!(req).header("Content-Type", "application/json").send(&json[..])
            }
            (m, None) => {
                let req = match m {
                    Method::Post => self.agent.post(&url),
                    Method::Put => self.agent.put(&url),
                    _ => self.agent.patch(&url),
                };
                headers!(req).send_empty()
            }
        };

        let response = result.map_err(transport_error)?;
        read(response)
    }
}

fn read(mut response: Response<ureq::Body>) -> Result<Value> {
    let status = response.status().as_u16();
    let bytes = response.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(transport_error)?;

    if !(200..300).contains(&status) {
        let body = String::from_utf8_lossy(&bytes).trim().to_string();
        return Err(status_error(status, &body));
    }

    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Object(Default::default()));
    }

    serde_json::from_slice(&bytes).or_else(|_| {
        // A non-JSON success body (plain-text logs, say) is still an answer; hand it back as
        // a string rather than failing a request the server said succeeded.
        Ok(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    })
}

fn transport_error(e: ureq::Error) -> Error {
    match e {
        ureq::Error::Timeout(_) => {
            Error::new(ErrorCode::Network, "The request timed out.").fix("Retry once, then stop.")
        }
        other => Error::new(ErrorCode::Network, "Could not reach the Sliplane API.")
            .detail(other.to_string())
            .fix("Retry once, then stop."),
    }
}

pub fn status_error(status: u16, body: &str) -> Error {
    let mut detail = format!("HTTP {status}");
    if !body.is_empty() {
        detail.push_str(": ");
        detail.push_str(body);
    }
    detail.push_str(hint(status, body));

    // Error bodies are `{code, message}`; the code says which of several 403s this is.
    let api_code = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("code").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();

    let e = match status {
        401 if api_code == "token_revoked" => Error::new(ErrorCode::AuthRequired, "The API key has been revoked.")
            .fix("Create a new key at https://sliplane.io/app/team/api, then: sliplane accounts add <name> --api-key <key> --force"),
        401 => Error::new(ErrorCode::AuthRequired, "The API key was rejected.")
            .fix("Replace it: sliplane accounts add <name> --api-key <key> --force"),
        403 if api_code == "read_only_token" => {
            Error::new(ErrorCode::AuthRequired, "The API key is read-only; only reads are allowed.")
                .fix("Use a read-write key: sliplane accounts add <name> --api-key <key> --force")
        }
        403 if api_code == "api_access_disabled" => {
            Error::new(ErrorCode::AuthRequired, "API access is not enabled for this organization.")
                .fix("Enable API access in the Sliplane team settings: https://sliplane.io/app/team/api")
        }
        403 if api_code == "feature_not_enabled" => {
            Error::new(ErrorCode::AuthRequired, "This feature or region is not enabled for the organization.")
                .fix("Ask Sliplane support (support@sliplane.io) to enable it.")
        }
        403 => Error::new(ErrorCode::AuthRequired, "The API key is not allowed to do that.").fix(
            "Check the key is read-write and belongs to the right organization. A legacy token also needs one: \
             sliplane accounts add <name> --api-key <key> --org-id <id> --force",
        ),
        404 => Error::new(ErrorCode::NotFound, "The resource does not exist."),
        429 => Error::new(ErrorCode::RateLimited, "Rate limited by the Sliplane API.")
            .fix("Back off before retrying."),
        400 | 422 => Error::new(ErrorCode::InvalidInput, "The API refused the request."),
        s if s >= 500 => Error::new(ErrorCode::Network, "The Sliplane API returned a server error.")
            .fix("Retry; if it persists the platform is having trouble."),
        _ => Error::new(ErrorCode::Error, "The request failed."),
    };
    e.detail(detail)
}

/// Turns the API's terser refusals into something actionable. The status and body are still
/// shown verbatim; this only appends what to do about it.
fn hint(status: u16, body: &str) -> &'static str {
    let lower = body.to_ascii_lowercase();
    if status == 409 && lower.contains("deployment") {
        return "\n\nA service cannot move between a registry image and a repository build. \
                Delete and recreate it with the deployment you want - volumes are server-level \
                resources and survive, so data on them is not lost.";
    }
    if status == 409 && lower.contains("project_not_empty") {
        return "\n\nA project must be empty before it can be deleted. Delete its services first \
                (sliplane services list --project-id <id>).";
    }
    if status == 400 && lower.contains("deployment configuration is required") {
        return "\n\nEvery update needs the deployment object. Pass --repo/--branch (or --image) \
                alongside whatever you are changing.";
    }
    ""
}

/// Percent-encodes one path segment, so an id or env key can never escape its place in the URL.
pub fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Builds `?a=1&b=2` from the pairs that have a value.
pub fn query(pairs: &[(&str, Option<String>)]) -> String {
    let parts: Vec<String> = pairs.iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", seg(v)))).collect();
    if parts.is_empty() { String::new() } else { format!("?{}", parts.join("&")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seg_escapes_reserved() {
        assert_eq!(seg("service_abc"), "service_abc");
        assert_eq!(seg("A B/C"), "A%20B%2FC");
    }

    #[test]
    fn query_skips_missing() {
        assert_eq!(query(&[("a", None), ("b", Some("1".into()))]), "?b=1");
        assert_eq!(query(&[("a", None)]), "");
        assert_eq!(
            query(&[("since", Some("2026-01-01T00:00:00+02:00".into()))]),
            "?since=2026-01-01T00%3A00%3A00%2B02%3A00"
        );
    }

    #[test]
    fn status_maps_to_codes() {
        assert_eq!(status_error(401, "").code, ErrorCode::AuthRequired);
        assert_eq!(status_error(404, "").code, ErrorCode::NotFound);
        assert_eq!(status_error(429, "").code, ErrorCode::RateLimited);
        assert_eq!(status_error(422, "").code, ErrorCode::InvalidInput);
        assert_eq!(status_error(503, "").code, ErrorCode::Network);
        assert_eq!(status_error(409, "").code, ErrorCode::Error);
        assert!(status_error(409, "deployment type").detail.unwrap().contains("recreate"));
        let ro = status_error(403, r#"{"code":"read_only_token","message":"read only"}"#);
        assert_eq!(ro.code, ErrorCode::AuthRequired);
        assert!(ro.message.contains("read-only"));
    }
}
