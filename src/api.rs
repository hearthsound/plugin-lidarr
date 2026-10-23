//! HTTP access to the Lidarr API through the `host-http` host import.
//!
//! This is the only module that touches the network. It owns the instance's
//! base URL and API key, builds the requests the Lidarr endpoints expect, and
//! maps a failing response to the provider error the server sees. The endpoints
//! themselves are composed by the modules that call this one.
//!
//! The [`LidarrApi`] trait is the seam the download logic is tested through: the
//! client implements it against HTTP, and the unit tests implement it against
//! recorded fixtures.

use hearthsound_plugin::host::http::{Request, Response};
use hearthsound_plugin::http;
use hearthsound_plugin::types::ProviderError;
use serde::de::DeserializeOwned;

use crate::config;
use crate::lidarr;
use crate::url;

/// Wall-clock budget for one Lidarr request, in milliseconds.
///
/// The host clamps this to its own ceiling and to the remaining call deadline.
const REQUEST_TIMEOUT_MS: u32 = 15_000;

/// How many history records one failure check reads.
const HISTORY_PAGE_SIZE: u32 = 100;

/// How many queue items one progress check reads.
const QUEUE_PAGE_SIZE: u32 = 200;

/// The Lidarr operations the acquisition logic needs.
///
/// Every method maps one endpoint. The trait exists so the sequence of an album
/// acquisition - ensure the artist, monitor the album, search, poll, check
/// history - can be tested without a Lidarr instance.
pub(crate) trait LidarrApi {
    /// Looks an album up by text or by `lidarr:<release-group id>`.
    ///
    /// Returns the lookup results, which may be empty.
    fn lookup_albums(&self, term: &str) -> Result<Vec<lidarr::Album>, ProviderError>;

    /// Reads one quality profile.
    fn quality_profile(&self, id: u32) -> Result<lidarr::QualityProfile, ProviderError>;

    /// Finds the artists Lidarr already holds for one MusicBrainz id.
    fn artists_by_mbid(&self, mbid: &str) -> Result<Vec<lidarr::Artist>, ProviderError>;

    /// Adds an artist to Lidarr's library.
    ///
    /// Returns the stored artist, whose id names it from then on.
    fn add_artist(&self, artist: &lidarr::AddArtist) -> Result<lidarr::Artist, ProviderError>;

    /// Finds the album Lidarr holds for one release-group id.
    fn albums_by_foreign_id(&self, foreign_id: &str) -> Result<Vec<lidarr::Album>, ProviderError>;

    /// Marks one album as monitored.
    fn monitor_album(&self, album_id: u32) -> Result<(), ProviderError>;

    /// Starts Lidarr's own search for one album.
    fn search_album(&self, album_id: u32) -> Result<(), ProviderError>;

    /// Reads the queue items of one album.
    fn queue(&self, album_id: u32) -> Result<Vec<lidarr::QueueItem>, ProviderError>;

    /// Reads the track files Lidarr imported for one album.
    fn track_files(&self, album_id: u32) -> Result<Vec<lidarr::TrackFile>, ProviderError>;

    /// Reports whether Lidarr recorded a failed download for one album at or
    /// after one instant.
    fn failed_download(&self, album_id: u32, since: Option<i64>) -> Result<bool, ProviderError>;
}

/// One Lidarr instance, as this provider instance is configured for it.
#[derive(Debug, Clone)]
pub(crate) struct Client {
    /// Base URL of the instance, trimmed and without a trailing slash.
    base_url: String,
    /// API key sent as `X-Api-Key` on every request.
    api_key: String,
}

impl Client {
    /// Builds a client for one instance.
    ///
    /// # Parameters
    ///
    /// - `config`: the instance configuration.
    pub(crate) fn new(config: &config::Config) -> Self {
        Self {
            base_url: config.base_url(),
            api_key: config.api_key.clone(),
        }
    }

    /// Reads the instance's system status.
    ///
    /// # Returns
    ///
    /// The Lidarr version the instance reports.
    ///
    /// # Errors
    ///
    /// Returns the mapped error of a failed request, or the mapped status of a
    /// non-success response, which is how `validate-config` and `health` tell a
    /// wrong URL or key from a working instance.
    pub(crate) fn health(&self) -> Result<String, ProviderError> {
        let status: lidarr::SystemStatus = self.get_json("/api/v1/system/status")?;

        Ok(status.version.unwrap_or_else(|| "unknown".to_string()))
    }

    /// Sends a GET request and decodes its JSON body.
    ///
    /// # Parameters
    ///
    /// - `path`: the API path, including any query string.
    ///
    /// # Returns
    ///
    /// The decoded response body.
    ///
    /// # Errors
    ///
    /// Returns the mapped status of a failing response, an error when the host
    /// cannot complete the request, and an internal error when the body is not
    /// JSON for `T`.
    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, ProviderError> {
        let response = self.send(&self.request("GET", path))?;
        decode_success(&response)
    }

    /// Sends a POST request with a JSON body and decodes its JSON response.
    ///
    /// # Parameters
    ///
    /// - `path`: the API path.
    /// - `body`: the value to send as the request body.
    ///
    /// # Returns
    ///
    /// The decoded response body.
    ///
    /// # Errors
    ///
    /// Returns the mapped status of a failing response, and an internal error
    /// when the body cannot be serialized or the response is not JSON.
    fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ProviderError> {
        let response = self.send(&self.with_body("POST", path, body)?)?;
        decode_success(&response)
    }

    /// Sends a PUT request with a JSON body, ignoring the response body.
    ///
    /// # Parameters
    ///
    /// - `path`: the API path.
    /// - `body`: the value to send as the request body.
    ///
    /// # Errors
    ///
    /// Returns the mapped status of a failing response, and an internal error
    /// when the body cannot be serialized.
    fn put_json<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<(), ProviderError> {
        let response = self.send(&self.with_body("PUT", path, body)?)?;
        require_success(&response)
    }

    /// Sends a POST request with a JSON body, ignoring the response body.
    ///
    /// # Parameters
    ///
    /// - `path`: the API path.
    /// - `body`: the value to send as the request body.
    ///
    /// # Errors
    ///
    /// Returns the mapped status of a failing response, and an internal error
    /// when the body cannot be serialized.
    fn post_ok<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<(), ProviderError> {
        let response = self.send(&self.with_body("POST", path, body)?)?;
        require_success(&response)
    }

    /// Sends a request and returns its response, whatever its status.
    ///
    /// # Parameters
    ///
    /// - `request`: the request to perform.
    ///
    /// # Returns
    ///
    /// The response.
    ///
    /// # Errors
    ///
    /// Returns the error the host reports: a denied request becomes
    /// unauthorized, and a timeout or network failure becomes unavailable.
    fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        http::send(request)
    }

    /// Builds a request for one API path.
    ///
    /// # Parameters
    ///
    /// - `method`: the HTTP method.
    /// - `path`: the API path, with or without a leading slash.
    ///
    /// # Returns
    ///
    /// The request, carrying the authentication header, the JSON accept header
    /// and the plugin's request timeout.
    fn request(&self, method: &str, path: &str) -> Request {
        Request {
            method: method.to_string(),
            url: url::endpoint(&self.base_url, path),
            headers: vec![
                ("accept".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), self.api_key.clone()),
            ],
            body: None,
            timeout_ms: Some(REQUEST_TIMEOUT_MS),
        }
    }

    /// Builds a request with a JSON body.
    ///
    /// # Parameters
    ///
    /// - `method`: the HTTP method.
    /// - `path`: the API path.
    /// - `body`: the value to serialize.
    ///
    /// # Returns
    ///
    /// The request, ready for [`Client::send`].
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Internal`] when the body cannot be serialized.
    fn with_body<B: serde::Serialize>(
        &self,
        method: &str,
        path: &str,
        body: &B,
    ) -> Result<Request, ProviderError> {
        let mut request = self.request(method, path);
        request.body = Some(
            serde_json::to_vec(body)
                .map_err(|error| ProviderError::Internal(format!("request body: {error}")))?,
        );
        request
            .headers
            .push(("content-type".to_string(), "application/json".to_string()));

        Ok(request)
    }
}

impl LidarrApi for Client {
    fn lookup_albums(&self, term: &str) -> Result<Vec<lidarr::Album>, ProviderError> {
        self.get_json(&format!(
            "/api/v1/album/lookup?term={}",
            url::encode_segment(term)
        ))
    }

    fn quality_profile(&self, id: u32) -> Result<lidarr::QualityProfile, ProviderError> {
        self.get_json(&format!("/api/v1/qualityprofile/{id}"))
    }

    fn artists_by_mbid(&self, mbid: &str) -> Result<Vec<lidarr::Artist>, ProviderError> {
        self.get_json(&format!(
            "/api/v1/artist?mbId={}",
            url::encode_segment(mbid)
        ))
    }

    fn add_artist(&self, artist: &lidarr::AddArtist) -> Result<lidarr::Artist, ProviderError> {
        self.post_json("/api/v1/artist", artist)
    }

    fn albums_by_foreign_id(&self, foreign_id: &str) -> Result<Vec<lidarr::Album>, ProviderError> {
        self.get_json(&format!(
            "/api/v1/album?foreignAlbumId={}",
            url::encode_segment(foreign_id)
        ))
    }

    fn monitor_album(&self, album_id: u32) -> Result<(), ProviderError> {
        self.put_json(
            "/api/v1/album/monitor",
            &lidarr::AlbumsMonitored {
                album_ids: vec![album_id],
                monitored: true,
            },
        )
    }

    fn search_album(&self, album_id: u32) -> Result<(), ProviderError> {
        self.post_ok(
            "/api/v1/command",
            &lidarr::AlbumSearchCommand {
                name: "AlbumSearch".to_string(),
                album_ids: vec![album_id],
            },
        )
    }

    fn queue(&self, album_id: u32) -> Result<Vec<lidarr::QueueItem>, ProviderError> {
        let page: lidarr::QueuePage =
            self.get_json(&format!("/api/v1/queue?page=1&pageSize={QUEUE_PAGE_SIZE}"))?;

        Ok(page
            .records
            .into_iter()
            .filter(|item| item.album_id == Some(album_id))
            .collect())
    }

    fn track_files(&self, album_id: u32) -> Result<Vec<lidarr::TrackFile>, ProviderError> {
        self.get_json(&format!("/api/v1/trackfile?albumId={album_id}"))
    }

    fn failed_download(&self, album_id: u32, since: Option<i64>) -> Result<bool, ProviderError> {
        let page: lidarr::HistoryPage = self.get_json(&format!(
            "/api/v1/history?albumId={album_id}&pageSize={HISTORY_PAGE_SIZE}"
        ))?;

        Ok(lidarr::has_failed_download(&page.records, since))
    }
}

/// Decodes a successful response body as JSON.
///
/// # Parameters
///
/// - `response`: the response to decode.
///
/// # Returns
///
/// The decoded body.
///
/// # Errors
///
/// Returns the mapped status of a non-success response and
/// [`ProviderError::Internal`] when the body is not JSON for `T`.
fn decode_success<T: DeserializeOwned>(response: &Response) -> Result<T, ProviderError> {
    require_success(response)?;

    serde_json::from_slice(&response.body)
        .map_err(|error| ProviderError::Internal(format!("response body: {error}")))
}

/// Requires a successful status, mapping the failure to a provider error.
///
/// # Parameters
///
/// - `response`: the response to check.
///
/// # Returns
///
/// `Ok(())` for a `2xx` status.
///
/// # Errors
///
/// Returns [`http::status_error`] of a non-`2xx` response.
fn require_success(response: &Response) -> Result<(), ProviderError> {
    if http::is_success(response) {
        Ok(())
    } else {
        Err(http::status_error(response))
    }
}

/// Renders a provider error as a sentence for a configuration problem.
///
/// # Parameters
///
/// - `error`: the error to describe.
///
/// # Returns
///
/// The message an administrator can act on.
pub(crate) fn describe(error: &ProviderError) -> String {
    match error {
        ProviderError::NotFound => "the instance answered not found".to_string(),
        ProviderError::RateLimited(ms) => {
            format!("the instance is rate limiting requests; retry after {ms} ms")
        }
        ProviderError::Unauthorized(message) => {
            format!("the instance rejected the API key: {message}")
        }
        ProviderError::InvalidConfig(message) => format!("invalid configuration: {message}"),
        ProviderError::Unavailable(message) => format!("the instance is unavailable: {message}"),
        ProviderError::Internal(message) => message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_a_rejected_key() {
        let error = ProviderError::Unauthorized("bad key".to_string());

        assert!(describe(&error).contains("bad key"));
    }
}
