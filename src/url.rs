//! URL validation and composition for the Lidarr API.
//!
//! Everything here is pure: it never touches the network and never depends on
//! the plugin bindings, so it is unit-tested on the host target.

/// The hexadecimal digits used by percent-encoding.
const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// Returns whether `value` is an absolute `http` or `https` URL.
///
/// The check is deliberately structural rather than a full URL parse: it
/// accepts exactly the shapes a Lidarr base URL can have (a service name on the
/// server's network, a host with a port, or a reverse proxy in front of one)
/// and rejects everything a request could not be built from.
///
/// # Parameters
///
/// - `value`: the candidate URL.
///
/// # Returns
///
/// `true` when `value` starts with `http://` or `https://`, carries a non-empty
/// host, and contains no whitespace.
///
/// # Example
///
/// ```text
/// is_http_url("http://lidarr:8686") -> true
/// is_http_url("lidarr:8686")        -> false
/// ```
pub fn is_http_url(value: &str) -> bool {
    let value = value.trim();
    let after_scheme = match value.strip_prefix("https://") {
        Some(rest) => rest,
        None => match value.strip_prefix("http://") {
            Some(rest) => rest,
            None => return false,
        },
    };
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    !host.is_empty() && !host.contains(char::is_whitespace)
}

/// Normalizes a configured base URL.
///
/// Surrounding whitespace and trailing slashes are removed so that the
/// configured value and the URLs built from it do not double up on `/`.
///
/// # Parameters
///
/// - `value`: the configured base URL.
///
/// # Returns
///
/// The trimmed URL, without a trailing slash.
///
/// # Example
///
/// ```text
/// normalize_base_url(" http://lidarr:8686/ ") -> "http://lidarr:8686"
/// ```
pub fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

/// Joins a base URL with an API path.
///
/// # Parameters
///
/// - `base_url`: the configured base URL, with or without a trailing slash.
/// - `path`: the API path, with or without a leading slash.
///
/// # Returns
///
/// The absolute URL of the endpoint.
///
/// # Example
///
/// ```text
/// endpoint("http://lidarr:8686/", "/api/v1/queue") -> "http://lidarr:8686/api/v1/queue"
/// ```
pub fn endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        normalize_base_url(base_url),
        path.trim_start_matches('/')
    )
}

/// Percent-encodes one query or path value.
///
/// Everything outside the RFC 3986 unreserved set is escaped, including `/`,
/// `?`, `#` and space, so a value cannot change the route or the query it is
/// pasted into.
///
/// # Parameters
///
/// - `value`: the value to encode.
///
/// # Returns
///
/// The encoded value.
///
/// # Example
///
/// ```text
/// encode_segment("Some Album [Deluxe]") -> "Some%20Album%20%5BDeluxe%5D"
/// ```
pub fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            _ => {
                encoded.push('%');
                encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_and_https_urls() {
        assert!(is_http_url("http://lidarr:8686"));
        assert!(is_http_url("https://music.example.com/lidarr"));
    }

    #[test]
    fn rejects_urls_without_a_host_or_scheme() {
        assert!(!is_http_url("lidarr:8686"));
        assert!(!is_http_url("ftp://lidarr"));
        assert!(!is_http_url("http://"));
        assert!(!is_http_url("http://two words"));
    }

    #[test]
    fn normalizes_a_base_url() {
        assert_eq!(
            normalize_base_url(" http://lidarr:8686/ "),
            "http://lidarr:8686"
        );
        assert_eq!(
            normalize_base_url("http://lidarr:8686"),
            "http://lidarr:8686"
        );
    }

    #[test]
    fn joins_an_endpoint() {
        assert_eq!(
            endpoint("http://lidarr:8686/", "/api/v1/queue"),
            "http://lidarr:8686/api/v1/queue"
        );
    }

    #[test]
    fn encodes_everything_outside_the_unreserved_set() {
        assert_eq!(
            encode_segment("Some Album [Deluxe]"),
            "Some%20Album%20%5BDeluxe%5D"
        );
        assert_eq!(encode_segment("a/b?c#d"), "a%2Fb%3Fc%23d");
        assert_eq!(encode_segment("azAZ09-_.~"), "azAZ09-_.~");
    }
}
