use crate::config::{AwsSecrets, S3Config};
use crate::store::ObjectStore;
use anyhow::{anyhow, bail, Context, Result};
use rusty_s3::actions::ListObjectsV2;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use std::io::Read;
use std::time::Duration;

const SIGN_TTL: Duration = Duration::from_secs(300);

/// Format a ureq error for logging WITHOUT leaking the (presigned) request URL.
///
/// `ureq::Error`'s `Display` writes the request URL for both variants
/// (`Status` via `response.get_url()`, `Transport` via its own `url` field),
/// and every URL we sign carries `X-Amz-Credential=<access key id>` plus a
/// replayable `X-Amz-Signature`. Never format `{e}` directly on an error
/// from a presigned request — use this instead.
fn s3_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _resp) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => format!("transport error: {}", t.kind()),
    }
}

/// Compare an S3 `ETag` response header against the MD5 of the uploaded body.
///
/// Returns `None` when the ETag is not a plain MD5 — SSE-KMS and multipart
/// uploads produce ETags that are *not* the object's MD5 (multipart ETags look
/// like `"<hex>-<partcount>"`), so a mismatch there is expected and must never
/// be treated as corruption. Only when the ETag is exactly 32 hex chars can we
/// verify it, and only then do we return `Some(true/false)`.
fn etag_matches(etag: &str, body: &[u8]) -> Option<bool> {
    let e = etag.trim().trim_matches('"').to_ascii_lowercase();
    if e.len() != 32 || !e.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{:x}", md5::compute(body)) == e)
}

pub struct S3ObjectStore {
    bucket: Bucket,
    creds: Credentials,
}

impl S3ObjectStore {
    pub fn new(cfg: &S3Config, creds: &AwsSecrets) -> Result<Self> {
        // Allow a MinIO/localstack override for tests; default to real AWS.
        let endpoint_override = std::env::var("GIT_ARK_S3_ENDPOINT").ok();
        let endpoint = match &endpoint_override {
            Some(e) => e.clone(),
            None => format!("https://s3.{}.amazonaws.com", cfg.region),
        };
        let style = if endpoint_override.is_some() {
            UrlStyle::Path // MinIO wants path-style
        } else {
            UrlStyle::VirtualHost
        };
        let bucket = Bucket::new(
            endpoint.parse().context("parsing S3 endpoint URL")?,
            style,
            cfg.bucket.clone(),
            cfg.region.clone(),
        )
        .map_err(|e| anyhow!("bucket config: {e}"))?;
        // A non-empty session_token means SSO/STS temporary creds (used by
        // `restore` on a trusted machine); static keys otherwise.
        let creds = match creds.session_token.as_deref().filter(|t| !t.is_empty()) {
            Some(token) => Credentials::new_with_token(
                creds.access_key_id.clone(),
                creds.secret_access_key.clone(),
                token.to_string(),
            ),
            None => Credentials::new(creds.access_key_id.clone(), creds.secret_access_key.clone()),
        };
        Ok(Self { bucket, creds })
    }
}

impl ObjectStore for S3ObjectStore {
    fn put(&self, key: &str, body: &[u8]) -> Result<()> {
        let action = self.bucket.put_object(Some(&self.creds), key);
        let url = action.sign(SIGN_TTL);
        let resp = ureq::put(url.as_str())
            .send_bytes(body)
            .map_err(|e| anyhow!("PutObject {key} failed: {}", s3_err(e)))?;
        if resp.status() >= 300 {
            bail!("PutObject {key}: HTTP {}", resp.status());
        }
        // Verify the upload against the returned ETag when it's a plain MD5.
        // `None` (SSE-KMS/multipart) and `Some(true)` proceed; only a proven
        // mismatch aborts — an undetected corrupt backup is unrecoverable.
        if let Some(etag) = resp.header("etag") {
            if etag_matches(etag, body) == Some(false) {
                bail!(
                    "PutObject {key}: upload integrity check failed \
                     (ETag != MD5) — possible corruption"
                );
            }
        }
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let action = self.bucket.get_object(Some(&self.creds), key);
        let url = action.sign(SIGN_TTL);
        let resp = ureq::get(url.as_str())
            .call()
            .map_err(|e| anyhow!("GetObject {key} failed: {}", s3_err(e)))?;
        if resp.status() >= 300 {
            bail!("GetObject {key}: HTTP {}", resp.status());
        }
        let mut buf = Vec::new();
        resp.into_reader().read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        // A single ListObjectsV2 response holds at most 1000 keys; loop on the
        // continuation token until the listing is no longer truncated so we
        // never silently drop history entries past the first page.
        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.creds));
            action.with_prefix(prefix);
            if let Some(token) = continuation.clone() {
                action.with_continuation_token(token);
            }
            let url = action.sign(SIGN_TTL);
            let resp = ureq::get(url.as_str())
                .call()
                .map_err(|e| anyhow!("ListObjectsV2 {prefix} failed: {}", s3_err(e)))?;
            if resp.status() >= 300 {
                bail!("ListObjectsV2 {prefix}: HTTP {}", resp.status());
            }
            let text = resp.into_string()?;
            let parsed = ListObjectsV2::parse_response(&text)
                .map_err(|e| anyhow!("parsing ListObjectsV2 response: {e}"))?;
            keys.extend(parsed.contents.into_iter().map(|o| o.key));
            match parsed.next_continuation_token {
                Some(token) if !token.is_empty() => continuation = Some(token),
                _ => break,
            }
        }
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_do_not_leak_presigned_url() {
        // Unreachable endpoint → transport error path.
        std::env::set_var("GIT_ARK_S3_ENDPOINT", "http://127.0.0.1:1");
        let cfg = S3Config {
            bucket: "b".into(),
            region: "us-east-1".into(),
            prefix: "git-ark".into(),
        };
        let creds = AwsSecrets {
            access_key_id: "AKIAEXAMPLELEAKCHECK".into(),
            secret_access_key: "s".into(),
            session_token: None,
        };
        let store = S3ObjectStore::new(&cfg, &creds).unwrap();
        let err = store.put("git-ark/x/latest.age", b"data").unwrap_err();
        std::env::remove_var("GIT_ARK_S3_ENDPOINT");
        let msg = format!("{err:#}");
        assert!(!msg.contains("X-Amz"), "leaked signed query params: {msg}");
        assert!(
            !msg.contains("AKIAEXAMPLELEAKCHECK"),
            "leaked access key id: {msg}"
        );
    }

    #[test]
    fn etag_matches_verifies_plain_md5_only() {
        // md5("hello") = 5d41402abc4b2a76b9719d911017c592
        let good = "5d41402abc4b2a76b9719d911017c592";
        assert_eq!(etag_matches(good, b"hello"), Some(true));
        // S3 wraps the ETag in double quotes — must still verify.
        assert_eq!(etag_matches(&format!("\"{good}\""), b"hello"), Some(true));
        // A valid-shaped 32-hex ETag that doesn't match → proven corruption.
        assert_eq!(
            etag_matches("00000000000000000000000000000000", b"hello"),
            Some(false)
        );
        // Multipart / SSE-KMS ETags are not 32-hex → unverifiable (None).
        assert_eq!(etag_matches("\"d41d8cd98f00b204e9800998ecf8427e-2\"", b"hello"), None);
        assert_eq!(etag_matches("not-a-hash", b"hello"), None);
    }
}
