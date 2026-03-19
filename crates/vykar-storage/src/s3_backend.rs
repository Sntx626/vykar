use std::io::Read;
use std::time::Duration;

use base64::Engine;
use md5::{Digest, Md5};
use percent_encoding::percent_decode_str;
use rusty_s3::actions::{ListObjectsV2, S3Action};
use rusty_s3::{Bucket, Credentials, UrlStyle};

use crate::retry::HttpRetryError;
use crate::RetryConfig;
use vykar_types::error::{Result, VykarError};

use crate::StorageBackend;

/// Duration for presigned URL validity.
const PRESIGN_DURATION: Duration = Duration::from_secs(3600);

pub struct S3Backend {
    bucket: Bucket,
    credentials: Credentials,
    agent: ureq::Agent,
    retry: RetryConfig,
    /// Prefix (root path) prepended to all keys.
    root: String,
    /// When true, `delete()` overwrites with a zero-byte tombstone instead of
    /// issuing a real DELETE. For S3 Object Lock compatibility.
    soft_delete: bool,
}

#[allow(clippy::result_large_err)]
impl S3Backend {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bucket_name: &str,
        region: &str,
        root: &str,
        endpoint: &str,
        access_key_id: &str,
        secret_access_key: &str,
        retry: RetryConfig,
        soft_delete: bool,
    ) -> Result<Self> {
        let base_url = endpoint.parse().map_err(|e| {
            VykarError::Config(format!("invalid S3 endpoint URL '{endpoint}': {e}"))
        })?;

        // Endpoint is always explicit in repository URL; use path-style addressing.
        let url_style = UrlStyle::Path;

        let bucket = Bucket::new(
            base_url,
            url_style,
            bucket_name.to_string(),
            region.to_string(),
        )
        .map_err(|e| VykarError::Config(format!("failed to create S3 bucket handle: {e}")))?;

        let credentials = Credentials::new(access_key_id, secret_access_key);

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_send_body(Some(Duration::from_secs(300)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .build()
            .into();

        // Normalize root: strip leading/trailing slashes, ensure trailing slash if non-empty.
        let root = root.trim_matches('/').to_string();

        Ok(Self {
            bucket,
            credentials,
            agent,
            retry,
            root,
            soft_delete,
        })
    }

    /// Prepend the root prefix to a key.
    fn full_key(&self, key: &str) -> String {
        if self.root.is_empty() {
            key.to_string()
        } else {
            format!("{}/{}", self.root, key)
        }
    }

    /// Unified retry wrapper for HTTP calls with response handling.
    fn retry_call<T>(
        &self,
        op_name: &str,
        f: impl Fn() -> std::result::Result<http::Response<ureq::Body>, ureq::Error>,
        handle_response: impl Fn(http::Response<ureq::Body>) -> std::result::Result<T, HttpRetryError>,
    ) -> std::result::Result<T, HttpRetryError> {
        crate::retry::retry_http(&self.retry, op_name, "S3", f, handle_response)
    }
}

/// Check an HTTP response status for S3 operations, reading the error body for
/// diagnostics on 4xx/5xx responses.
///
/// Returns `Ok(())` for success status codes (< 400). For error statuses,
/// reads the S3 XML error body before classifying for retry.
fn s3_check_status(
    resp: &mut http::Response<ureq::Body>,
    op: &str,
    key: &str,
) -> std::result::Result<(), HttpRetryError> {
    let status = resp.status().as_u16();
    if status < 400 {
        return Ok(());
    }
    // Read error body for diagnostics — S3 returns XML with error details.
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    let truncated;
    let display_body = if body.len() > 1024 {
        truncated = format!("{}...(truncated)", &body[..body.floor_char_boundary(1024)]);
        &truncated
    } else {
        &body
    };
    tracing::debug!("S3 {op} {key}: HTTP {status}: {display_body}");
    crate::retry::classify_status(status, format!("HTTP {status}: {display_body}"))
}

/// Convert an [`HttpRetryError`] into a `VykarError` for S3 operations.
fn s3_error(op: &str, key: &str, err: HttpRetryError) -> VykarError {
    VykarError::Other(format!("S3 {op} {key}: {err}"))
}

#[allow(clippy::result_large_err)]
impl StorageBackend for S3Backend {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let full_key = self.full_key(key);
        let url = self
            .bucket
            .get_object(Some(&self.credentials), &full_key)
            .sign(PRESIGN_DURATION);

        let soft_delete = self.soft_delete;
        self.retry_call(
            &format!("GET {key}"),
            || self.agent.get(url.as_str()).call(),
            |mut resp| {
                let status = resp.status().as_u16();
                if status == 404 {
                    return Ok(None);
                }
                s3_check_status(&mut resp, "GET", key)?;
                let mut buf = Vec::new();
                resp.body_mut()
                    .as_reader()
                    .read_to_end(&mut buf)
                    .map_err(HttpRetryError::BodyIo)?;
                // Treat zero-byte objects as tombstones (soft-deleted).
                if soft_delete && buf.is_empty() {
                    return Ok(None);
                }
                Ok(Some(buf))
            },
        )
        .map_err(|e| s3_error("GET", key, e))
    }

    fn put(&self, key: &str, data: &[u8]) -> Result<()> {
        self.put_bytes(key, data)
    }

    fn put_owned(&self, key: &str, data: Vec<u8>) -> Result<()> {
        self.put_bytes(key, &data)
    }

    fn delete(&self, key: &str) -> Result<()> {
        if self.soft_delete {
            // Overwrite with a zero-byte tombstone instead of deleting.
            // With S3 Object Lock + versioning, the previous version is
            // preserved for the configured retention period.
            return self.put_bytes(key, &[]);
        }
        let full_key = self.full_key(key);
        let url = self
            .bucket
            .delete_object(Some(&self.credentials), &full_key)
            .sign(PRESIGN_DURATION);

        self.retry_call(
            &format!("DELETE {key}"),
            || self.agent.delete(url.as_str()).call(),
            |mut resp| s3_check_status(&mut resp, "DELETE", key),
        )
        .map_err(|e| s3_error("DELETE", key, e))?;
        Ok(())
    }

    fn exists(&self, key: &str) -> Result<bool> {
        let full_key = self.full_key(key);
        let url = self
            .bucket
            .head_object(Some(&self.credentials), &full_key)
            .sign(PRESIGN_DURATION);

        let soft_delete = self.soft_delete;
        self.retry_call(
            &format!("HEAD {key}"),
            || self.agent.head(url.as_str()).call(),
            |mut resp| {
                let status = resp.status().as_u16();
                if status == 404 {
                    return Ok(false);
                }
                s3_check_status(&mut resp, "HEAD", key)?;
                if soft_delete {
                    let len = crate::http_util::extract_content_length(
                        resp.headers(),
                        &format!("S3 HEAD {key}"),
                    )
                    .map_err(|e| HttpRetryError::Permanent(e.to_string()))?;
                    Ok(len > 0)
                } else {
                    Ok(true)
                }
            },
        )
        .map_err(|e| s3_error("HEAD", key, e))
    }

    fn size(&self, key: &str) -> Result<Option<u64>> {
        let full_key = self.full_key(key);
        let url = self
            .bucket
            .head_object(Some(&self.credentials), &full_key)
            .sign(PRESIGN_DURATION);

        let soft_delete = self.soft_delete;
        self.retry_call(
            &format!("HEAD {key}"),
            || self.agent.head(url.as_str()).call(),
            |mut resp| {
                let status = resp.status().as_u16();
                if status == 404 {
                    return Ok(None);
                }
                s3_check_status(&mut resp, "HEAD", key)?;
                let len = crate::http_util::extract_content_length(
                    resp.headers(),
                    &format!("S3 HEAD {key}"),
                )
                .map_err(|e| HttpRetryError::Permanent(e.to_string()))?;
                // Treat zero-byte objects as tombstones (soft-deleted).
                if soft_delete && len == 0 {
                    return Ok(None);
                }
                Ok(Some(len))
            },
        )
        .map_err(|e| s3_error("HEAD", key, e))
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let full_prefix = self.full_key(prefix);
        let root_prefix_len = if self.root.is_empty() {
            0
        } else {
            self.root.len() + 1 // +1 for the '/'
        };

        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
            action.query_mut().insert("prefix", &full_prefix);
            if let Some(ref token) = continuation_token {
                action.query_mut().insert("continuation-token", token);
            }
            let url = action.sign(PRESIGN_DURATION);

            let parsed = self
                .retry_call(
                    &format!("LIST {prefix}"),
                    || self.agent.get(url.as_str()).call(),
                    |mut resp| {
                        s3_check_status(&mut resp, "LIST", prefix)?;
                        let mut body = Vec::new();
                        resp.body_mut()
                            .as_reader()
                            .read_to_end(&mut body)
                            .map_err(HttpRetryError::BodyIo)?;
                        ListObjectsV2::parse_response(&body).map_err(|e| {
                            HttpRetryError::Permanent(format!(
                                "S3 LIST {prefix}: failed to parse response: {e}"
                            ))
                        })
                    },
                )
                .map_err(|e| s3_error("LIST", prefix, e))?;

            for obj in &parsed.contents {
                // rusty_s3 sends encoding-type=url; some S3-compatible backends
                // (e.g. Garage) URL-encode keys in the response. Decode here —
                // for backends that don't encode, this is a no-op.
                let key = percent_decode_str(&obj.key)
                    .decode_utf8()
                    .map_err(|e| VykarError::Other(format!("S3 LIST: invalid UTF-8 in key: {e}")))?
                    .into_owned();
                // Skip directory markers
                if key.ends_with('/') {
                    continue;
                }
                // Skip zero-byte tombstones (soft-deleted objects).
                if self.soft_delete && obj.size == 0 {
                    continue;
                }
                // Strip root prefix to return relative keys
                if root_prefix_len > 0 && key.len() > root_prefix_len {
                    keys.push(key[root_prefix_len..].to_string());
                } else {
                    keys.push(key);
                }
            }

            match parsed.next_continuation_token {
                Some(token) => continuation_token = Some(token),
                None => break,
            }
        }

        Ok(keys)
    }

    fn get_range(&self, key: &str, offset: u64, length: u64) -> Result<Option<Vec<u8>>> {
        if length == 0 {
            return Err(VykarError::Other(format!(
                "S3 GET_RANGE {key}: zero-length read requested"
            )));
        }
        // Tombstone check: a zero-byte object cannot satisfy a range read.
        if self.soft_delete && self.size(key)?.is_none() {
            return Ok(None);
        }
        let full_key = self.full_key(key);
        let end = offset
            .checked_add(length)
            .and_then(|n| n.checked_sub(1))
            .ok_or_else(|| {
                VykarError::Other(format!(
                    "S3 GET_RANGE {key}: offset {offset} + length {length} overflows u64"
                ))
            })?;
        let range_header = format!("bytes={offset}-{end}");

        let mut action = self.bucket.get_object(Some(&self.credentials), &full_key);
        // SigV4 canonicalizes signed header names as lowercase.
        // Use lowercase here so the presigned SignedHeaders list is compliant.
        action.headers_mut().insert("range", &range_header);
        let url = action.sign(PRESIGN_DURATION);

        self.retry_call(
            &format!("GET_RANGE {key}"),
            || {
                self.agent
                    .get(url.as_str())
                    .header("range", &range_header)
                    .call()
            },
            |mut resp| {
                let status = resp.status().as_u16();
                if status == 404 {
                    return Ok(None);
                }
                if status >= 400 {
                    s3_check_status(&mut resp, "GET_RANGE", key)?;
                }
                if status == 200 {
                    return Err(HttpRetryError::Permanent(format!(
                        "S3 GET_RANGE {key}: server returned 200 instead of 206 (Range header ignored)"
                    )));
                }
                if status != 206 {
                    return Err(HttpRetryError::Permanent(format!(
                        "S3 GET_RANGE {key}: unexpected status {status}"
                    )));
                }
                let cap = match usize::try_from(length) {
                    Ok(c) => c,
                    Err(_) => {
                        return Err(HttpRetryError::Permanent(format!(
                            "S3 GET_RANGE {key}: length {length} exceeds platform usize"
                        )));
                    }
                };
                let mut buf = Vec::with_capacity(cap);
                resp.body_mut()
                    .as_reader()
                    .take(length)
                    .read_to_end(&mut buf)
                    .map_err(HttpRetryError::BodyIo)?;
                if buf.len() != cap {
                    return Err(HttpRetryError::BodyIo(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!(
                            "short read on {key} at offset {offset}: expected {length} bytes, got {}",
                            buf.len()
                        ),
                    )));
                }
                Ok(Some(buf))
            },
        )
        .map_err(|e| s3_error("GET_RANGE", key, e))
    }

    fn create_dir(&self, key: &str) -> Result<()> {
        let dir_key = if key.ends_with('/') {
            self.full_key(key)
        } else {
            self.full_key(&format!("{key}/"))
        };
        let content_type = "application/octet-stream";
        let content_md5 = base64::engine::general_purpose::STANDARD.encode(Md5::digest(b""));

        let mut action = self.bucket.put_object(Some(&self.credentials), &dir_key);
        action.headers_mut().insert("content-type", content_type);
        action.headers_mut().insert("content-md5", &content_md5);
        let url = action.sign(PRESIGN_DURATION);

        self.retry_call(
            &format!("MKDIR {key}"),
            || {
                self.agent
                    .put(url.as_str())
                    .header("content-type", content_type)
                    .header("content-md5", &content_md5)
                    .send(&[] as &[u8])
            },
            |mut resp| s3_check_status(&mut resp, "MKDIR", key),
        )
        .map_err(|e| s3_error("MKDIR", key, e))?;
        Ok(())
    }
}

#[allow(clippy::result_large_err)]
impl S3Backend {
    fn put_bytes(&self, key: &str, data: &[u8]) -> Result<()> {
        let full_key = self.full_key(key);
        let content_type = "application/octet-stream";
        // Content-MD5 is required for S3 buckets with Object Lock enabled.
        let content_md5 = base64::engine::general_purpose::STANDARD.encode(Md5::digest(data));

        let mut action = self.bucket.put_object(Some(&self.credentials), &full_key);
        // Sign content-type and content-md5 so the presigned URL covers the
        // headers the HTTP client sends with the body.
        action.headers_mut().insert("content-type", content_type);
        action.headers_mut().insert("content-md5", &content_md5);
        let url = action.sign(PRESIGN_DURATION);

        self.retry_call(
            &format!("PUT {key}"),
            || {
                self.agent
                    .put(url.as_str())
                    .header("content-type", content_type)
                    .header("content-md5", &content_md5)
                    .send(data)
            },
            |mut resp| s3_check_status(&mut resp, "PUT", key),
        )
        .map_err(|e| s3_error("PUT", key, e))?;
        Ok(())
    }
}
