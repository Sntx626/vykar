use vykar_types::error::{Result, VykarError};

/// Extract and parse the `Content-Length` header from HTTP response headers.
pub fn extract_content_length(headers: &http::HeaderMap, context: &str) -> Result<u64> {
    let header = headers.get(http::header::CONTENT_LENGTH).ok_or_else(|| {
        VykarError::Other(format!("{context}: response missing Content-Length header"))
    })?;
    let val = header
        .to_str()
        .map_err(|_| VykarError::Other(format!("{context}: non-ASCII Content-Length header")))?;
    val.parse::<u64>()
        .map_err(|_| VykarError::Other(format!("{context}: invalid Content-Length header: {val}")))
}
