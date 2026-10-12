//! Shared HTTP plumbing: one configured agent and a helper that reads whole
//! response bodies.

use std::time::Duration;

use anyhow::{Context, Result};
use ureq::http::{Request, Response};

/// Largest response body we accept. Large enough for asset downloads.
pub(crate) const MAX_BODY_BYTES: u64 = 512 * 1024 * 1024;

/// Builds the blocking agent. HTTP error statuses are returned as responses
/// rather than errors so callers can read Roblox's error bodies.
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
}

/// Sends one request and reads the whole body into memory.
pub(crate) fn execute(agent: &ureq::Agent, request: Request<Vec<u8>>) -> Result<Response<Vec<u8>>> {
    let label = format!("{} {}", request.method(), request.uri().path());
    let response = agent.run(request).with_context(|| format!("request to {label} failed"))?;
    let (parts, mut body) = response.into_parts();
    let bytes = body
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()
        .with_context(|| format!("reading response from {label} failed"))?;
    Ok(Response::from_parts(parts, bytes))
}

/// Returns the response body as text, truncated for use in error messages.
pub(crate) fn body_snippet(response: &Response<Vec<u8>>) -> String {
    const LIMIT: usize = 500;
    let text = String::from_utf8_lossy(response.body());
    let trimmed = text.trim();
    if trimmed.chars().count() > LIMIT {
        let cut: String = trimmed.chars().take(LIMIT).collect();
        format!("{cut}...")
    } else {
        trimmed.to_owned()
    }
}

/// Reads a numeric header such as `x-ratelimit-reset`, given in seconds.
pub(crate) fn seconds_header(response: &Response<Vec<u8>>, name: &str) -> Option<Duration> {
    let value = response.headers().get(name)?.to_str().ok()?;
    let seconds: f64 = value.trim().parse().ok()?;
    // Clamp so an absurd header cannot make `from_secs_f64` panic.
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds.min(86_400.0)))
}
