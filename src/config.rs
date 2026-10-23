//! The plugin's server-scoped configuration.
//!
//! The manifest carries the JSON Schema the admin UI renders; this module is
//! the plugin's own view of the same document. It has two halves:
//!
//! - [`Config`], the typed configuration the call path reads, with the defaults
//!   the manifest declares;
//! - [`check`], the schema-level validation `validate-config` runs before it
//!   asks the configured instance anything.
//!
//! Everything here is pure: it never touches the network and never depends on
//! the plugin bindings beyond the shared error type, so it is unit-tested on
//! the host target.

use crate::url;

/// Lidarr base URL used when the instance configuration omits one.
pub const DEFAULT_URL: &str = "http://lidarr:8686";

/// The server-scoped configuration of one plugin instance.
///
/// Every field has a default, because the plugin must stay callable while an
/// administrator is still filling the configuration in; [`check`] is what
/// refuses an unusable document before it is stored.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Base URL of the Lidarr instance.
    #[serde(default = "default_url")]
    pub url: String,
    /// Lidarr API key, sent as `X-Api-Key`.
    #[serde(default)]
    pub api_key: String,
    /// Id of the import root that exposes the root folder to the server.
    #[serde(default)]
    pub import_root: String,
    /// Lidarr's root folder path, as Lidarr reports track file paths.
    #[serde(default)]
    pub root_folder_path: String,
    /// Quality profile a newly added artist is stored with.
    #[serde(default = "default_profile")]
    pub quality_profile_id: u32,
    /// Metadata profile a newly added artist is stored with.
    #[serde(default = "default_profile")]
    pub metadata_profile_id: u32,
    /// How long a download may wait to appear in Lidarr's queue.
    #[serde(default = "default_search_wait_minutes")]
    pub search_wait_minutes: u32,
}

impl Config {
    /// Deserializes an instance configuration document.
    ///
    /// # Parameters
    ///
    /// - `json`: the JSON document `host-config` returned, or the one
    ///   `validate-config` was handed.
    ///
    /// # Returns
    ///
    /// The typed configuration, with the manifest defaults for every field the
    /// document omits.
    ///
    /// # Errors
    ///
    /// Returns the message describing the problem when the document is not JSON
    /// or a field does not have the type the manifest declares.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| format!("server config: {error}"))
    }

    /// Returns the instance's base URL without a trailing slash.
    pub fn base_url(&self) -> String {
        url::normalize_base_url(&self.url)
    }

    /// Returns the root folder path without a trailing slash.
    pub fn root_folder(&self) -> String {
        self.root_folder_path
            .trim()
            .trim_end_matches('/')
            .to_string()
    }
}

/// Returns the default base URL of the Lidarr instance.
fn default_url() -> String {
    DEFAULT_URL.to_string()
}

/// Returns the default profile id.
fn default_profile() -> u32 {
    1
}

/// Returns the default search wait.
fn default_search_wait_minutes() -> u32 {
    30
}

/// Returns the schema-level problems of a configuration document.
///
/// These are the checks `validate-config` can make without asking the instance
/// anything: the document parses, the URL is one a request can be built from,
/// and the key, the import root and the root folder path are present.
///
/// # Parameters
///
/// - `json`: the JSON document to check.
///
/// # Returns
///
/// One message per problem, in field order; empty when the document passes. A
/// document that is not JSON at all yields exactly one message.
///
/// # Example
///
/// ```text
/// check(r#"{"url":"lidarr:8686","apiKey":"k"}"#)
///     -> ["url must be an http or https URL, got `lidarr:8686`", ...]
/// ```
pub fn check(json: &str) -> Vec<String> {
    let Ok(document) = serde_json::from_str::<serde_json::Value>(json) else {
        return vec!["server config is not valid JSON".to_string()];
    };

    let mut problems = Vec::new();
    check_url(&document, &mut problems);
    check_text(&document, "apiKey", "apiKey", &mut problems);
    check_text(&document, "importRoot", "importRoot", &mut problems);
    check_text(&document, "rootFolderPath", "rootFolderPath", &mut problems);
    check_positive(&document, "qualityProfileId", &mut problems);
    check_positive(&document, "metadataProfileId", &mut problems);
    check_positive(&document, "searchWaitMinutes", &mut problems);

    problems
}

/// Checks the `url` member of a configuration document.
///
/// # Parameters
///
/// - `document`: the configuration document.
/// - `problems`: collects the messages.
fn check_url(document: &serde_json::Value, problems: &mut Vec<String>) {
    match text(document, "url") {
        Some(url) if url::is_http_url(url) => {}
        Some(url) => problems.push(format!("url must be an http or https URL, got `{url}`")),
        None => problems.push("url must be an http or https URL".to_string()),
    }
}

/// Checks one required string field, appending a problem when it is missing or
/// empty.
///
/// # Parameters
///
/// - `document`: the configuration document.
/// - `field`: the JSON member to read.
/// - `label`: the name the problem message uses, which is the member name.
/// - `problems`: collects the messages.
fn check_text(document: &serde_json::Value, field: &str, label: &str, problems: &mut Vec<String>) {
    match text(document, field) {
        Some(value) if !value.trim().is_empty() => {}
        Some(_) => problems.push(format!("{label} must not be empty")),
        None => problems.push(format!("{label} is required")),
    }
}

/// Checks one positive integer field, which may be absent but not zero.
///
/// # Parameters
///
/// - `document`: the configuration document.
/// - `field`: the JSON member to read.
/// - `problems`: collects the messages.
fn check_positive(document: &serde_json::Value, field: &str, problems: &mut Vec<String>) {
    match document.get(field).and_then(serde_json::Value::as_u64) {
        Some(0) => problems.push(format!("{field} must be at least 1")),
        None if document.get(field).is_some_and(|value| !value.is_null()) => {
            problems.push(format!("{field} must be an integer"));
        }
        _ => {}
    }
}

/// Reads a string member of a configuration document.
///
/// # Parameters
///
/// - `document`: the configuration document.
/// - `field`: the JSON member to read.
///
/// # Returns
///
/// The member's string value, and `None` when the member is absent, `null`, or
/// not a string.
fn text<'a>(document: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    document.get(field).and_then(serde_json::Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_complete_configuration() {
        assert_eq!(
            check(
                r#"{"url":"http://lidarr:8686","apiKey":"secret","importRoot":"lidarr","rootFolderPath":"/music"}"#
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn reports_a_document_that_is_not_json() {
        assert_eq!(
            check("not json"),
            vec!["server config is not valid JSON".to_string()]
        );
    }

    #[test]
    fn reports_every_missing_field_at_once() {
        let problems = check("{}");

        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(
            problems[0].contains("url must be an http or https URL"),
            "{}",
            problems[0]
        );
        assert!(
            problems[1].contains("apiKey is required"),
            "{}",
            problems[1]
        );
        assert!(
            problems[2].contains("importRoot is required"),
            "{}",
            problems[2]
        );
        assert!(
            problems[3].contains("rootFolderPath is required"),
            "{}",
            problems[3]
        );
    }

    #[test]
    fn reports_empty_field_values() {
        let problems = check(
            r#"{"url":"http://lidarr:8686","apiKey":"  ","importRoot":"","rootFolderPath":" "}"#,
        );

        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("apiKey must not be empty"));
        assert!(problems[1].contains("importRoot must not be empty"));
        assert!(problems[2].contains("rootFolderPath must not be empty"));
    }

    #[test]
    fn reports_a_url_that_is_not_http() {
        let problems = check(
            r#"{"url":"lidarr:8686","apiKey":"secret","importRoot":"root","rootFolderPath":"/music"}"#,
        );

        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("url must be an http or https URL"));
        assert!(problems[0].contains("lidarr:8686"));
    }

    #[test]
    fn reports_zero_profile_and_wait_values() {
        let problems = check(
            r#"{"url":"http://lidarr:8686","apiKey":"k","importRoot":"r","rootFolderPath":"/m","qualityProfileId":0,"metadataProfileId":0,"searchWaitMinutes":0}"#,
        );

        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("qualityProfileId must be at least 1"));
        assert!(problems[1].contains("metadataProfileId must be at least 1"));
        assert!(problems[2].contains("searchWaitMinutes must be at least 1"));
    }

    #[test]
    fn uses_the_manifest_defaults() {
        let config = Config::from_json("{}").unwrap();

        assert_eq!(config.url, DEFAULT_URL);
        assert_eq!(config.base_url(), "http://lidarr:8686");
        assert_eq!(config.quality_profile_id, 1);
        assert_eq!(config.metadata_profile_id, 1);
        assert_eq!(config.search_wait_minutes, 30);
        assert!(config.api_key.is_empty());
        assert!(config.import_root.is_empty());
        assert_eq!(config.root_folder(), "");
    }

    #[test]
    fn reads_the_configured_values() {
        let config = Config::from_json(
            r#"{
                "url": "http://lidarr:8686/",
                "apiKey": "secret",
                "importRoot": "lidarr",
                "rootFolderPath": "/music/",
                "qualityProfileId": 4,
                "metadataProfileId": 2,
                "searchWaitMinutes": 45
            }"#,
        )
        .unwrap();

        assert_eq!(config.base_url(), "http://lidarr:8686");
        assert_eq!(config.api_key, "secret");
        assert_eq!(config.import_root, "lidarr");
        assert_eq!(config.root_folder(), "/music");
        assert_eq!(config.quality_profile_id, 4);
        assert_eq!(config.metadata_profile_id, 2);
        assert_eq!(config.search_wait_minutes, 45);
    }
}
