//! Roblox Open Cloud: asset uploads and place publishing, authenticated with an
//! API key in the `x-api-key` header.

use std::fmt;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use ureq::http::{Method, Request, Response};

use crate::http::{agent, body_snippet, execute, seconds_header};
use crate::multipart::{self, Part};

/// Asset type string sent for [`AssetKind::Mesh`].
///
/// UNVERIFIED: this value has not been confirmed against the live API. It is
/// kept in this single constant so it can be corrected in one place.
pub const MESH_ASSET_TYPE: &str = "Mesh";

/// Upper bound on the wait between operation polls.
const MAX_POLL_DELAY: Duration = Duration::from_secs(30);
/// Number of operation polls before giving up.
const MAX_POLLS: u32 = 10;
/// Retries after a 429 response; the request is sent at most six times.
const MAX_RATE_LIMIT_RETRIES: u32 = 5;
/// Longest display name Open Cloud accepts, in characters.
const MAX_DISPLAY_NAME_CHARS: usize = 50;
const UPLOAD_DESCRIPTION: &str = "Uploaded with Kahua";

/// The user or group that owns uploaded assets and their universe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Creator {
    User(u64),
    Group(u64),
}

impl Creator {
    fn to_json(self) -> Value {
        match self {
            Creator::User(id) => json!({ "userId": id.to_string() }),
            Creator::Group(id) => json!({ "groupId": id.to_string() }),
        }
    }
}

/// Kinds of asset this client can upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// `.png`, `.jpg`/`.jpeg`, `.bmp` or `.tga`.
    Image,
    /// `.glb`, `.gltf`, `.fbx` or `.rbxm`.
    Model,
    /// Roblox-format `.mesh` bytes. The content type is always
    /// `application/octet-stream`.
    Mesh,
}

/// Delays used by the retry loops. The defaults match Open Cloud guidance;
/// tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// First delay before polling an upload operation. Doubles on each poll.
    pub poll_initial: Duration,
    /// Delay before retrying a 429 response that has no `x-ratelimit-reset`.
    /// Doubles on each retry.
    pub rate_limit_backoff: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self { poll_initial: Duration::from_secs(1), rate_limit_backoff: Duration::from_secs(1) }
    }
}

/// Client for Open Cloud. The API key is never shown by `Debug`.
pub struct OpenCloud {
    api_key: String,
    creator: Creator,
    base_url: String,
    timing: Timing,
    agent: ureq::Agent,
}

impl fmt::Debug for OpenCloud {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenCloud")
            .field("api_key", &"<redacted>")
            .field("creator", &self.creator)
            .field("base_url", &self.base_url)
            .field("timing", &self.timing)
            .finish_non_exhaustive()
    }
}

impl OpenCloud {
    /// Creates a client for [`crate::OPEN_CLOUD_BASE_URL`].
    pub fn new(api_key: impl Into<String>, creator: Creator) -> Self {
        Self {
            api_key: api_key.into(),
            creator,
            base_url: crate::OPEN_CLOUD_BASE_URL.to_owned(),
            timing: Timing::default(),
            agent: agent(),
        }
    }

    /// Replaces the base URL, for example with a local mock server.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_owned();
        self
    }

    /// Replaces the retry delays.
    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    /// Uploads an asset and waits for Open Cloud to finish processing it.
    ///
    /// The content type is chosen from `file_name`'s extension for
    /// [`AssetKind::Image`] and [`AssetKind::Model`]. Returns the new asset id.
    pub fn upload_asset(&self, kind: AssetKind, display_name: &str, file_name: &str, bytes: &[u8]) -> Result<u64> {
        let (asset_type, content_type) = asset_type_and_content_type(kind, file_name)?;
        let display_name = display_name_for(display_name, file_name);
        let file_name = if file_name.trim().is_empty() { "asset" } else { file_name };

        let request_json = json!({
            "assetType": asset_type,
            "displayName": display_name,
            "description": UPLOAD_DESCRIPTION,
            "creationContext": { "creator": self.creator.to_json() },
        })
        .to_string();

        let (multipart_type, body) = multipart::encode(&[
            Part { name: "request", filename: None, content_type: "application/json", data: request_json.as_bytes() },
            Part { name: "fileContent", filename: Some(file_name), content_type, data: bytes },
        ]);

        let url = format!("{}/assets/v1/assets", self.base_url);
        let response = self.send(|| {
            self.api_request(Method::POST, &url)?
                .header("content-type", multipart_type.as_str())
                .body(body.clone())
                .map_err(Into::into)
        })?;
        check_status(&response)?;

        let json = parse_json(&response, "upload response")?;
        let operation_id = json
            .get("operationId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                json.get("path").and_then(Value::as_str).and_then(|p| p.strip_prefix("operations/")).map(str::to_owned)
            })
            .ok_or_else(|| anyhow!("Open Cloud upload response had no operation id"))?;

        self.wait_for_asset(&operation_id)
    }

    /// Publishes a place version and returns its version number.
    ///
    /// `xml` selects `application/xml` (`.rbxlx`); otherwise the body is sent as
    /// `application/octet-stream` (`.rbxl`).
    pub fn publish_place(&self, universe_id: u64, place_id: u64, bytes: &[u8], xml: bool) -> Result<u64> {
        let content_type = if xml { "application/xml" } else { "application/octet-stream" };
        let url =
            format!("{}/universes/v1/{universe_id}/places/{place_id}/versions?versionType=Published", self.base_url);
        let response = self.send(|| {
            self.api_request(Method::POST, &url)?
                .header("content-type", content_type)
                .body(bytes.to_vec())
                .map_err(Into::into)
        })?;
        check_status(&response)?;

        let json = parse_json(&response, "publish response")?;
        json.get("versionNumber")
            .and_then(parse_id)
            .ok_or_else(|| anyhow!("Open Cloud publish response had no versionNumber"))
    }

    fn wait_for_asset(&self, operation_id: &str) -> Result<u64> {
        if !is_safe_path_segment(operation_id) {
            bail!("Open Cloud returned an operation id with unexpected characters");
        }
        let url = format!("{}/assets/v1/operations/{operation_id}", self.base_url);
        let mut delay = self.timing.poll_initial;

        for _ in 0..MAX_POLLS {
            thread::sleep(delay);
            let response = self.send(|| self.api_request(Method::GET, &url)?.body(Vec::new()).map_err(Into::into))?;
            check_status(&response)?;
            let json = parse_json(&response, "operation status")?;

            if let Some(error) = json.get("error").filter(|e| !e.is_null()) {
                bail!("Open Cloud could not create the asset: {}", describe_error(error));
            }
            if json.get("done").and_then(Value::as_bool).unwrap_or(false) {
                return json
                    .get("response")
                    .and_then(|r| r.get("assetId"))
                    .and_then(parse_id)
                    .ok_or_else(|| anyhow!("Open Cloud finished operation {operation_id} without an asset id"));
            }
            delay = (delay * 2).min(MAX_POLL_DELAY);
        }

        bail!(
            "Timed out waiting for Open Cloud operation {operation_id} after {MAX_POLLS} polls; \
             it may still finish later"
        )
    }

    fn api_request(&self, method: Method, url: &str) -> Result<ureq::http::request::Builder> {
        let key = ureq::http::HeaderValue::from_str(&self.api_key)
            .map_err(|_| anyhow!("The API key contains characters that are not allowed in a header"))?;
        Ok(Request::builder().method(method).uri(url).header("x-api-key", key))
    }

    /// Sends a request, retrying on 429 up to [`MAX_RATE_LIMIT_RETRIES`] times.
    fn send(&self, make: impl Fn() -> Result<Request<Vec<u8>>>) -> Result<Response<Vec<u8>>> {
        let mut retries = 0;
        loop {
            let response = execute(&self.agent, make()?)?;
            if response.status().as_u16() != 429 || retries >= MAX_RATE_LIMIT_RETRIES {
                return Ok(response);
            }
            let wait = seconds_header(&response, "x-ratelimit-reset")
                .unwrap_or(self.timing.rate_limit_backoff * 2u32.pow(retries));
            log::warn!("Open Cloud rate limit reached; retrying in {wait:?}");
            thread::sleep(wait);
            retries += 1;
        }
    }
}

fn asset_type_and_content_type(kind: AssetKind, file_name: &str) -> Result<(&'static str, &'static str)> {
    let extension =
        Path::new(file_name).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();

    match kind {
        AssetKind::Image => {
            let content_type = match extension.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "bmp" => "image/bmp",
                "tga" => "image/tga",
                _ => bail!("'{file_name}' is not a supported image; use png, jpg, bmp or tga"),
            };
            Ok(("Image", content_type))
        }
        AssetKind::Model => {
            let content_type = match extension.as_str() {
                "glb" => "model/gltf-binary",
                "gltf" => "model/gltf+json",
                "fbx" => "model/fbx",
                "rbxm" => "model/x-rbxm",
                _ => bail!("'{file_name}' is not a supported model; use glb, gltf, fbx or rbxm"),
            };
            Ok(("Model", content_type))
        }
        AssetKind::Mesh => Ok((MESH_ASSET_TYPE, "application/octet-stream")),
    }
}

fn display_name_for(display_name: &str, file_name: &str) -> String {
    let name = if display_name.trim().is_empty() { file_name } else { display_name };
    name.chars().take(MAX_DISPLAY_NAME_CHARS).collect()
}

/// Operation ids and other path segments must not be able to change the URL.
fn is_safe_path_segment(segment: &str) -> bool {
    !segment.is_empty() && segment.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Accepts ids sent either as JSON strings (`"123"`) or numbers.
fn parse_id(value: &Value) -> Option<u64> {
    match value {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

fn describe_error(error: &Value) -> String {
    error.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| error.to_string())
}

fn parse_json(response: &Response<Vec<u8>>, what: &str) -> Result<Value> {
    serde_json::from_slice(response.body())
        .with_context(|| format!("Open Cloud {what} was not valid JSON: {}", body_snippet(response)))
}

fn check_status(response: &Response<Vec<u8>>) -> Result<()> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(match status {
        401 => anyhow!("API key was rejected (401). Check that it has the Assets read/write permission"),
        403 => anyhow!(
            "API key is not allowed to do this (403). Check its permissions and that it covers \
             the creator or universe"
        ),
        429 => anyhow!(
            "Open Cloud is still rate limiting requests (429) after {MAX_RATE_LIMIT_RETRIES} \
             retries; try again later"
        ),
        _ => anyhow!("Open Cloud returned HTTP {status}: {}", body_snippet(response)),
    })
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn image_and_model_content_types_come_from_extension() {
        assert_eq!(asset_type_and_content_type(AssetKind::Image, "a.PNG").unwrap(), ("Image", "image/png"));
        assert_eq!(asset_type_and_content_type(AssetKind::Image, "a.jpeg").unwrap(), ("Image", "image/jpeg"));
        assert_eq!(asset_type_and_content_type(AssetKind::Model, "rig.glb").unwrap(), ("Model", "model/gltf-binary"));
        assert_eq!(asset_type_and_content_type(AssetKind::Model, "rig.rbxm").unwrap(), ("Model", "model/x-rbxm"));
        assert!(asset_type_and_content_type(AssetKind::Image, "a.exe").is_err());
        assert!(asset_type_and_content_type(AssetKind::Model, "a.png").is_err());
    }

    #[test]
    fn mesh_uses_constant_and_octet_stream() {
        assert_eq!(
            asset_type_and_content_type(AssetKind::Mesh, "x.mesh").unwrap(),
            (MESH_ASSET_TYPE, "application/octet-stream")
        );
    }

    #[test]
    fn display_name_is_truncated_to_fifty_characters() {
        let long = "é".repeat(80);
        assert_eq!(display_name_for(&long, "f.png").chars().count(), 50);
        assert_eq!(display_name_for("  ", "f.png"), "f.png");
    }

    #[test]
    fn path_segments_are_checked() {
        assert!(is_safe_path_segment("abc-123_x"));
        assert!(!is_safe_path_segment("../etc"));
        assert!(!is_safe_path_segment(""));
    }

    #[test]
    fn ids_accept_strings_and_numbers() {
        assert_eq!(parse_id(&json!("123")), Some(123));
        assert_eq!(parse_id(&json!(45)), Some(45));
        assert_eq!(parse_id(&json!("x")), None);
    }

    #[test]
    fn debug_hides_api_key() {
        let client = OpenCloud::new("super-secret-key", Creator::User(1));
        let text = format!("{client:?}");
        assert!(!text.contains("super-secret-key"));
        assert!(text.contains("<redacted>"));
    }
}
