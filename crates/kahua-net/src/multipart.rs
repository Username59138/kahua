//! Minimal `multipart/form-data` encoder for the Open Cloud asset upload.

use std::time::{SystemTime, UNIX_EPOCH};

/// One form part.
pub(crate) struct Part<'a> {
    pub name: &'a str,
    /// Set for file parts; sent as the `filename` parameter.
    pub filename: Option<&'a str>,
    pub content_type: &'a str,
    pub data: &'a [u8],
}

/// Encodes the parts. Returns the `Content-Type` header value and the body.
pub(crate) fn encode(parts: &[Part<'_>]) -> (String, Vec<u8>) {
    let boundary = choose_boundary(parts);
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", part.name);
        if let Some(filename) = part.filename {
            disposition.push_str(&format!("; filename=\"{}\"", sanitize(filename)));
        }
        disposition.push_str("\r\n");
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", part.content_type).as_bytes());
        body.extend_from_slice(part.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Strips characters that would break the quoted `filename` parameter.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '"' | '\\' | '\r' | '\n' => '_',
            other => other,
        })
        .collect()
}

/// Picks a boundary that does not occur in any part's data.
fn choose_boundary(parts: &[Part<'_>]) -> String {
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut counter: u64 = 0;
    loop {
        let candidate = format!("kahua{:x}{:x}", seed, counter);
        let needle = candidate.as_bytes();
        if parts.iter().all(|p| !contains(p.data, needle)) {
            return candidate;
        }
        counter += 1;
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_fields_and_file() {
        let parts = [
            Part {
                name: "request",
                filename: None,
                content_type: "application/json",
                data: br#"{"assetType":"Image"}"#,
            },
            Part {
                name: "fileContent",
                filename: Some("pic\".png"),
                content_type: "image/png",
                data: b"\x89PNG\r\n\x1a\n",
            },
        ];
        let (content_type, body) = encode(&parts);
        let boundary = content_type.strip_prefix("multipart/form-data; boundary=").expect("content type has boundary");
        let text = String::from_utf8_lossy(&body).into_owned();

        assert!(text.starts_with(&format!("--{boundary}\r\n")));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
        assert!(text.contains("Content-Disposition: form-data; name=\"request\"\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n\r\n{\"assetType\":\"Image\"}\r\n"));
        assert!(text.contains("name=\"fileContent\"; filename=\"pic_.png\"\r\n"));
        assert!(text.contains("Content-Type: image/png\r\n\r\n"));
        // Binary data must pass through byte for byte.
        assert!(body.windows(4).any(|w| w == b"\x89PNG"));
        // Two parts plus the closing delimiter.
        assert_eq!(text.matches(&format!("--{boundary}")).count(), 3);
    }
}
