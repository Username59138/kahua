//! Network tests. Every request goes to a tiny HTTP server on 127.0.0.1, so
//! nothing here touches Roblox.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use ureq::http::Method;

use crate::{AssetKind, Creator, OpenCloud, Session, Timing, User};

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    /// Path and query, as sent.
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn path(&self) -> &str {
        self.url.split('?').next().unwrap_or("")
    }
}

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn json(status: u16, value: Value) -> Self {
        Self { status, headers: Vec::new(), body: value.to_string().into_bytes() }
    }

    fn raw(status: u16, body: Vec<u8>) -> Self {
        Self { status, headers: Vec::new(), body }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

struct Mock {
    base: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    /// Starts a server. `handler` gets each request and the server's base URL.
    fn start(handler: impl Fn(&Recorded, &str) -> Reply + Send + Sync + 'static) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind mock server");
        let port = server.server_addr().to_ip().expect("ip address").port();
        let base = format!("http://127.0.0.1:{port}");
        let requests = Arc::new(Mutex::new(Vec::new()));

        let log = Arc::clone(&requests);
        let thread_base = base.clone();
        thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).expect("read request body");
                let headers = request
                    .headers()
                    .iter()
                    .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
                    .collect();
                let recorded =
                    Recorded { method: request.method().to_string(), url: request.url().to_string(), headers, body };
                log.lock().unwrap().push(recorded.clone());

                let reply = handler(&recorded, &thread_base);
                let mut response = tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
                for (name, value) in reply.headers {
                    response.add_header(
                        tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header"),
                    );
                }
                let _ = request.respond(response);
            }
        });

        Self { base, requests }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

fn fast_timing() -> Timing {
    Timing { poll_initial: Duration::from_millis(1), rate_limit_backoff: Duration::from_millis(1) }
}

fn client(base: &str) -> OpenCloud {
    OpenCloud::new("test-key", Creator::Group(456)).with_base_url(base).with_timing(fast_timing())
}

/// Splits a `multipart/form-data` body into `(headers, data)` pairs.
fn multipart_parts(content_type: &str, body: &[u8]) -> Vec<(String, Vec<u8>)> {
    let boundary = content_type.strip_prefix("multipart/form-data; boundary=").expect("multipart content type");
    let delimiter = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    for piece in split_on(body, &delimiter).into_iter().skip(1) {
        if piece.starts_with(b"--") {
            break;
        }
        let piece = piece.strip_prefix(b"\r\n").expect("CRLF after delimiter");
        let piece = piece.strip_suffix(b"\r\n").expect("CRLF before delimiter");
        let split = find(piece, b"\r\n\r\n").expect("header terminator");
        let headers = String::from_utf8(piece[..split].to_vec()).expect("utf-8 headers");
        parts.push((headers, piece[split + 4..].to_vec()));
    }
    parts
}

fn split_on<'a>(haystack: &'a [u8], needle: &[u8]) -> Vec<&'a [u8]> {
    let mut pieces = Vec::new();
    let mut rest = haystack;
    while let Some(at) = find(rest, needle) {
        pieces.push(&rest[..at]);
        rest = &rest[at + needle.len()..];
    }
    pieces.push(rest);
    pieces
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn boundary_of(request: &Recorded) -> String {
    request.header("content-type").expect("content type").to_owned()
}

fn expect_err<T: std::fmt::Debug>(result: anyhow::Result<T>) -> String {
    format!("{:#}", result.expect_err("expected an error"))
}

#[test]
fn upload_sends_multipart_and_polls_operation() {
    let polls = AtomicUsize::new(0);
    let mock = Mock::start(move |req, _base| match (req.method.as_str(), req.path()) {
        ("POST", "/assets/v1/assets") => {
            Reply::json(200, json!({ "path": "operations/op-1", "operationId": "op-1", "done": false }))
        }
        ("GET", "/assets/v1/operations/op-1") => {
            if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::json(200, json!({ "done": false }))
            } else {
                Reply::json(200, json!({ "done": true, "response": { "assetId": "987654321" } }))
            }
        }
        other => panic!("unexpected request {other:?}"),
    });

    let file_bytes = b"\x89PNG\r\n\x1a\nfake-image".to_vec();
    let long_name = "n".repeat(80);
    let asset_id = client(&mock.base).upload_asset(AssetKind::Image, &long_name, "logo.png", &file_bytes).unwrap();
    assert_eq!(asset_id, 987_654_321);

    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "one upload and two polls");
    let upload = &requests[0];
    assert_eq!(upload.method, "POST");
    assert_eq!(upload.header("x-api-key"), Some("test-key"));
    assert!(boundary_of(upload).starts_with("multipart/form-data; boundary="));

    let parts = multipart_parts(&boundary_of(upload), &upload.body);
    assert_eq!(parts.len(), 2);

    let (request_headers, request_data) = &parts[0];
    assert!(request_headers.contains("name=\"request\""));
    let request_json: Value = serde_json::from_slice(request_data).unwrap();
    assert_eq!(request_json["assetType"], "Image");
    assert_eq!(request_json["creationContext"]["creator"]["groupId"], "456");
    assert_eq!(request_json["displayName"].as_str().unwrap().chars().count(), 50);

    let (file_headers, file_data) = &parts[1];
    assert!(file_headers.contains("name=\"fileContent\"; filename=\"logo.png\""));
    assert!(file_headers.contains("Content-Type: image/png"));
    assert_eq!(file_data, &file_bytes);

    for poll in &requests[1..] {
        assert_eq!(poll.method, "GET");
        assert_eq!(poll.header("x-api-key"), Some("test-key"));
    }
}

#[test]
fn user_creator_and_mesh_upload_use_expected_fields() {
    let mock = Mock::start(|req, _| match req.path() {
        "/assets/v1/assets" => Reply::json(200, json!({ "operationId": "op-m" })),
        _ => Reply::json(200, json!({ "done": true, "response": { "assetId": 5 } })),
    });
    let id = OpenCloud::new("k", Creator::User(99))
        .with_base_url(&mock.base)
        .with_timing(fast_timing())
        .upload_asset(AssetKind::Mesh, "", "thing.mesh", b"mesh-bytes")
        .unwrap();
    assert_eq!(id, 5);

    let requests = mock.requests();
    let parts = multipart_parts(&boundary_of(&requests[0]), &requests[0].body);
    let request_json: Value = serde_json::from_slice(&parts[0].1).unwrap();
    assert_eq!(request_json["assetType"], "Mesh");
    assert_eq!(request_json["creationContext"]["creator"]["userId"], "99");
    // Empty display name falls back to the file name.
    assert_eq!(request_json["displayName"], "thing.mesh");
    assert!(parts[1].0.contains("Content-Type: application/octet-stream"));
}

#[test]
fn operation_error_is_surfaced() {
    let mock = Mock::start(|req, _| match req.path() {
        "/assets/v1/assets" => Reply::json(200, json!({ "operationId": "op-e" })),
        _ => Reply::json(200, json!({ "done": true, "error": { "code": 3, "message": "Image is corrupt" } })),
    });
    let message = expect_err(client(&mock.base).upload_asset(AssetKind::Image, "a", "a.png", b"x"));
    assert!(message.contains("Image is corrupt"), "{message}");
}

#[test]
fn polling_gives_up_after_ten_polls() {
    let mock = Mock::start(|req, _| match req.path() {
        "/assets/v1/assets" => Reply::json(200, json!({ "operationId": "op-slow" })),
        _ => Reply::json(200, json!({ "done": false })),
    });
    let message = expect_err(client(&mock.base).upload_asset(AssetKind::Image, "a", "a.png", b"x"));
    assert!(message.contains("Timed out"), "{message}");
    let polls = mock.requests().iter().filter(|r| r.method == "GET").count();
    assert_eq!(polls, 10);
}

#[test]
fn rate_limited_request_is_retried() {
    let posts = AtomicUsize::new(0);
    let mock = Mock::start(move |req, _| match req.path() {
        "/assets/v1/assets" => {
            if posts.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::json(429, json!({ "message": "slow down" })).with_header("x-ratelimit-reset", "0")
            } else {
                Reply::json(200, json!({ "operationId": "op-r" }))
            }
        }
        _ => Reply::json(200, json!({ "done": true, "response": { "assetId": "7" } })),
    });
    let id = client(&mock.base).upload_asset(AssetKind::Image, "a", "a.jpg", b"x").unwrap();
    assert_eq!(id, 7);
    let uploads = mock.requests().iter().filter(|r| r.path() == "/assets/v1/assets").count();
    assert_eq!(uploads, 2);
}

#[test]
fn rate_limit_retries_are_capped_at_five() {
    let mock = Mock::start(|_, _| Reply::json(429, json!({})).with_header("x-ratelimit-reset", "0"));
    let message = expect_err(client(&mock.base).publish_place(1, 2, b"x", false));
    assert!(message.contains("429"), "{message}");
    assert_eq!(mock.requests().len(), 6, "one attempt plus five retries");
}

#[test]
fn rate_limit_without_reset_header_uses_backoff() {
    let posts = AtomicUsize::new(0);
    let mock = Mock::start(move |_, _| {
        if posts.fetch_add(1, Ordering::SeqCst) < 2 {
            Reply::json(429, json!({}))
        } else {
            Reply::json(200, json!({ "versionNumber": 3 }))
        }
    });
    let version = client(&mock.base).publish_place(1, 2, b"x", false).unwrap();
    assert_eq!(version, 3);
    assert_eq!(mock.requests().len(), 3);
}

#[test]
fn publish_place_sends_raw_body_and_query() {
    let mock = Mock::start(|_, _| Reply::json(200, json!({ "versionNumber": 7 })));
    let client = client(&mock.base);

    let binary = client.publish_place(111, 222, b"\x1f\x8bbinary", false).unwrap();
    assert_eq!(binary, 7);
    let xml = client.publish_place(111, 222, b"<roblox/>", true).unwrap();
    assert_eq!(xml, 7);

    let requests = mock.requests();
    let first = &requests[0];
    assert_eq!(first.method, "POST");
    assert_eq!(first.path(), "/universes/v1/111/places/222/versions");
    assert_eq!(first.url.split('?').nth(1), Some("versionType=Published"));
    assert_eq!(first.header("content-type"), Some("application/octet-stream"));
    assert_eq!(first.header("x-api-key"), Some("test-key"));
    assert_eq!(first.body, b"\x1f\x8bbinary");

    let second = &requests[1];
    assert_eq!(second.header("content-type"), Some("application/xml"));
    assert_eq!(second.body, b"<roblox/>");
}

#[test]
fn rejected_key_message_is_helpful_and_hides_key() {
    let mock = Mock::start(|_, _| Reply::json(401, json!({ "message": "Unauthorized" })));
    let message = expect_err(client(&mock.base).publish_place(1, 2, b"x", false));
    assert!(message.contains("API key was rejected (401)"), "{message}");
    assert!(message.contains("Assets read/write permission"), "{message}");
    assert!(!message.contains("test-key"), "{message}");
}

#[test]
fn other_open_cloud_errors_include_status_and_body() {
    let mock = Mock::start(|_, _| Reply::raw(500, b"boom from server".to_vec()));
    let message = expect_err(client(&mock.base).publish_place(1, 2, b"x", false));
    assert!(message.contains("500"), "{message}");
    assert!(message.contains("boom from server"), "{message}");
}

#[test]
fn csrf_token_is_fetched_and_request_retried_once() {
    let mock = Mock::start(|req, _| {
        if req.header("x-csrf-token") == Some("tok-1") {
            Reply::json(200, json!({ "ok": true }))
        } else {
            Reply::json(403, json!({})).with_header("x-csrf-token", "tok-1")
        }
    });
    let session = Session::new("cookie-value").with_base_urls(&mock.base, &mock.base);

    let response = session
        .request(Method::POST, &format!("{}/thing", mock.base), b"{}".to_vec(), Some("application/json"))
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("x-csrf-token"), None);
    assert_eq!(requests[1].header("x-csrf-token"), Some("tok-1"));
    assert_eq!(requests[1].header("cookie"), Some(".ROBLOSECURITY=cookie-value"));
    assert_eq!(requests[1].header("content-type"), Some("application/json"));

    // The token is remembered for the next request, so no extra round trip.
    let again = session.request(Method::PATCH, &format!("{}/thing", mock.base), Vec::new(), None).unwrap();
    assert_eq!(again.status().as_u16(), 200);
    assert_eq!(mock.requests().len(), 3);
}

#[test]
fn csrf_retry_happens_only_for_mutating_methods_and_once() {
    let always_403 = Mock::start(|_, _| Reply::json(403, json!({})).with_header("x-csrf-token", "t"));
    let session = Session::new("c").with_base_urls(&always_403.base, &always_403.base);

    // GET never retries with a token.
    let get = session.request(Method::GET, &format!("{}/g", always_403.base), Vec::new(), None).unwrap();
    assert_eq!(get.status().as_u16(), 403);
    assert_eq!(always_403.requests().len(), 1);

    // POST retries exactly once, then returns the second response.
    let post = session.request(Method::POST, &format!("{}/p", always_403.base), Vec::new(), None).unwrap();
    assert_eq!(post.status().as_u16(), 403);
    assert_eq!(always_403.requests().len(), 3);
}

#[test]
fn download_follows_location_and_gunzips_without_cookie() {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"mesh-payload").unwrap();
    let gzipped = encoder.finish().unwrap();

    let mock = Mock::start(move |req, base| match req.path() {
        "/v2/assetId/42" => {
            Reply::json(200, json!({ "locations": [{ "location": format!("{base}/cdn/42"), "assetFormat": "mesh" }] }))
        }
        "/cdn/42" => Reply::raw(200, gzipped.clone()).with_header("content-encoding", "gzip"),
        other => panic!("unexpected {other}"),
    });
    let session = Session::new("abc").with_base_urls(&mock.base, &mock.base);

    let bytes = session.download_asset(42).unwrap();
    assert_eq!(bytes, b"mesh-payload");

    let requests = mock.requests();
    assert_eq!(requests[0].header("cookie"), Some(".ROBLOSECURITY=abc"));
    assert_eq!(requests[1].header("cookie"), None, "CDN gets no cookie");
}

#[test]
fn download_returns_plain_bytes_unchanged() {
    let mock = Mock::start(|req, base| match req.path() {
        "/v2/assetId/1" => Reply::json(200, json!({ "locations": [{ "location": format!("{base}/f") }] })),
        _ => Reply::raw(200, b"plain \x00 bytes".to_vec()),
    });
    let session = Session::new("abc").with_base_urls(&mock.base, &mock.base);
    assert_eq!(session.download_asset(1).unwrap(), b"plain \x00 bytes");
}

#[test]
fn download_reports_top_level_errors() {
    let mock =
        Mock::start(|_, _| Reply::json(200, json!({ "errors": [{ "code": 1, "message": "Asset is not approved" }] })));
    let session = Session::new("abc").with_base_urls(&mock.base, &mock.base);
    let message = expect_err(session.download_asset(9));
    assert!(message.contains("Asset is not approved"), "{message}");
}

#[test]
fn download_rejects_missing_location() {
    let mock = Mock::start(|_, _| Reply::json(200, json!({ "locations": [] })));
    let session = Session::new("abc").with_base_urls(&mock.base, &mock.base);
    let message = expect_err(session.download_asset(9));
    assert!(message.contains("no download location"), "{message}");
}

#[test]
fn current_user_parses_account() {
    let mock = Mock::start(|req, _| {
        if req.header("cookie") == Some(".ROBLOSECURITY=good") {
            Reply::json(200, json!({ "id": 1, "name": "tester", "displayName": "Tester" }))
        } else {
            Reply::json(401, json!({ "errors": [] }))
        }
    });

    let user = Session::new("good").with_base_urls(&mock.base, &mock.base).current_user().unwrap();
    assert_eq!(user, User { id: 1, name: "tester".to_owned(), display_name: "Tester".to_owned() });
}

#[test]
fn rejected_cookie_message_is_helpful_and_hides_cookie() {
    let mock = Mock::start(|_, _| Reply::json(401, json!({})));
    let session = Session::new("secret-cookie-value").with_base_urls(&mock.base, &mock.base);
    let message = expect_err(session.current_user());
    assert!(message.contains("Roblox cookie was rejected (401)"), "{message}");
    assert!(message.contains("fresh .ROBLOSECURITY cookie"), "{message}");
    assert!(!message.contains("secret-cookie-value"), "{message}");
}

#[test]
fn clients_hide_secrets_in_debug_output() {
    let session = Session::new("secret-cookie-value");
    let text = format!("{session:?}");
    assert!(!text.contains("secret-cookie-value"));
    assert!(text.contains("<redacted>"));
}
