//! Cookie session: authenticated requests with a `.ROBLOSECURITY` cookie.
//!
//! Modeled on Tarmac's `roblox_web_api.rs`: POST, PATCH and DELETE requests
//! answered with 403 and an `x-csrf-token` header are retried once with the
//! token.

use std::fmt;
use std::io::Read;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::Value;
use ureq::http::{HeaderValue, Method, Request, Response};

use crate::http::{agent, body_snippet, execute};

/// The account a cookie belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: u64,
    pub name: String,
    pub display_name: String,
}

/// An authenticated Roblox web session. The cookie is never shown by `Debug`.
pub struct Session {
    cookie: String,
    csrf: Mutex<Option<String>>,
    asset_delivery_url: String,
    users_url: String,
    agent: ureq::Agent,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("cookie", &"<redacted>")
            .field("asset_delivery_url", &self.asset_delivery_url)
            .field("users_url", &self.users_url)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Creates a session for the raw cookie value (the part after
    /// `.ROBLOSECURITY=`).
    pub fn new(cookie: impl Into<String>) -> Self {
        Self {
            cookie: cookie.into(),
            csrf: Mutex::new(None),
            asset_delivery_url: crate::ASSET_DELIVERY_BASE_URL.to_owned(),
            users_url: crate::USERS_BASE_URL.to_owned(),
            agent: agent(),
        }
    }

    /// Replaces the asset delivery and users base URLs, for example with a
    /// local mock server.
    pub fn with_base_urls(mut self, asset_delivery: impl Into<String>, users: impl Into<String>) -> Self {
        self.asset_delivery_url = asset_delivery.into().trim_end_matches('/').to_owned();
        self.users_url = users.into().trim_end_matches('/').to_owned();
        self
    }

    /// Returns the account the cookie belongs to. Use this to check a cookie.
    pub fn current_user(&self) -> Result<User> {
        let url = format!("{}/v1/users/authenticated", self.users_url);
        let response = self.request(Method::GET, &url, Vec::new(), None)?;
        check_status(&response, "account lookup")?;
        serde_json::from_slice(response.body())
            .with_context(|| format!("Roblox account response was not valid: {}", body_snippet(&response)))
    }

    /// Downloads an asset's bytes, decompressing gzip data when present.
    pub fn download_asset(&self, id: u64) -> Result<Vec<u8>> {
        let url = format!("{}/v2/assetId/{id}", self.asset_delivery_url);
        let response = self.request(Method::GET, &url, Vec::new(), None)?;
        check_status(&response, &format!("asset {id}"))?;

        let json: Value = serde_json::from_slice(response.body()).with_context(|| {
            format!("asset delivery response for {id} was not valid JSON: {}", body_snippet(&response))
        })?;

        let errors = json.get("errors").and_then(Value::as_array);
        if let Some(errors) = errors.filter(|list| !list.is_empty()) {
            let messages: Vec<String> = errors
                .iter()
                .map(|e| e.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| e.to_string()))
                .collect();
            bail!("Roblox could not deliver asset {id}: {}", messages.join("; "));
        }

        let location = json
            .get("locations")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .and_then(|entry| entry.get("location"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Roblox returned no download location for asset {id}"))?;
        if !(location.starts_with("https://") || location.starts_with("http://")) {
            bail!("Roblox returned an unexpected download location for asset {id}");
        }

        // The CDN does not need the cookie, so the request is sent without it.
        let request = Request::builder()
            .method(Method::GET)
            .uri(location)
            .body(Vec::new())
            .context("building the download request failed")?;
        let download = execute(&self.agent, request)?;
        if !download.status().is_success() {
            bail!("download of asset {id} failed with HTTP {}", download.status().as_u16());
        }

        let bytes = download.into_body();
        if bytes.starts_with(&GZIP_MAGIC) {
            let mut decoded = Vec::new();
            GzDecoder::new(bytes.as_slice())
                .read_to_end(&mut decoded)
                .with_context(|| format!("decompressing asset {id} failed"))?;
            Ok(decoded)
        } else {
            Ok(bytes)
        }
    }

    /// Sends an authenticated request.
    ///
    /// POST, PATCH and DELETE requests that get a 403 with an `x-csrf-token`
    /// header are retried once with that token. The token is kept for later
    /// requests.
    pub fn request(
        &self,
        method: Method,
        url: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<Response<Vec<u8>>> {
        let needs_csrf = matches!(method, Method::POST | Method::PATCH | Method::DELETE);
        let response = self.send_once(&method, url, &body, content_type)?;
        if !needs_csrf || response.status().as_u16() != 403 {
            return Ok(response);
        }
        let Some(token) = response.headers().get("x-csrf-token").and_then(|v| v.to_str().ok()).map(str::to_owned)
        else {
            return Ok(response);
        };

        *self.csrf_guard() = Some(token);
        self.send_once(&method, url, &body, content_type)
    }

    fn send_once(
        &self,
        method: &Method,
        url: &str,
        body: &[u8],
        content_type: Option<&str>,
    ) -> Result<Response<Vec<u8>>> {
        let cookie = HeaderValue::from_str(&format!(".ROBLOSECURITY={}", self.cookie))
            .map_err(|_| anyhow!("the Roblox cookie contains characters that are not allowed in a header"))?;
        let mut builder = Request::builder().method(method.clone()).uri(url).header("cookie", cookie);
        if let Some(token) = self.csrf_guard().clone() {
            let token = HeaderValue::from_str(&token).map_err(|_| anyhow!("Roblox returned an invalid CSRF token"))?;
            builder = builder.header("x-csrf-token", token);
        }
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        let request = builder.body(body.to_vec()).context("building the request failed")?;
        execute(&self.agent, request)
    }

    fn csrf_guard(&self) -> MutexGuard<'_, Option<String>> {
        self.csrf.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

fn check_status(response: &Response<Vec<u8>>, what: &str) -> Result<()> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(match status {
        401 => anyhow!(
            "Roblox cookie was rejected (401) while fetching {what}. Log into Roblox Studio again, \
             or provide a fresh .ROBLOSECURITY cookie"
        ),
        403 => anyhow!("Roblox refused {what} (403). This account may not have access to it"),
        404 => anyhow!("Roblox could not find {what} (404)"),
        _ => anyhow!("Roblox returned HTTP {status} while fetching {what}: {}", body_snippet(response)),
    })
}
