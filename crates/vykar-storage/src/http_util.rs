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

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(name: &str, value: &str) -> http::HeaderMap {
        let mut map = http::HeaderMap::new();
        map.insert(
            http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            http::header::HeaderValue::from_str(value).unwrap(),
        );
        map
    }

    #[test]
    fn valid_content_length() {
        let headers = headers_with("content-length", "42");
        assert_eq!(extract_content_length(&headers, "test").unwrap(), 42);
    }

    #[test]
    fn zero_content_length() {
        let headers = headers_with("content-length", "0");
        assert_eq!(extract_content_length(&headers, "test").unwrap(), 0);
    }

    #[test]
    fn missing_content_length() {
        let headers = http::HeaderMap::new();
        let err = extract_content_length(&headers, "test")
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing Content-Length"), "got: {err}");
    }

    #[test]
    fn non_numeric_content_length() {
        let headers = headers_with("content-length", "garbage");
        let err = extract_content_length(&headers, "test")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid Content-Length"), "got: {err}");
    }

    #[test]
    fn negative_content_length() {
        let headers = headers_with("content-length", "-1");
        let err = extract_content_length(&headers, "test")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid Content-Length"), "got: {err}");
    }
}
