//! Lidarr acquisition plugin: a Hearthsound acquisition provider over a Lidarr
//! instance.
//!
//! The plugin drives one [Lidarr](https://lidarr.audio) instance, which owns the
//! indexer search, the download client and the import into a root folder. It
//! implements `lifecycle` (configuration validation and a health probe) and
//! `acquisition` at album granularity: the server hands the plugin an album and
//! its wanted tracks, the plugin looks the album up, makes sure Lidarr has its
//! artist, monitors the album and asks Lidarr for its search, then reports the
//! files Lidarr imported once the album's tracks that map to the listed wanted
//! tracks are present inside the configured import root. Only lookup results
//! that name the requested album become candidates, and a candidate lists the
//! wanted tracks the release's own tracklist holds when Lidarr sent one.
//!
//! The plugin never deletes or modifies a file Lidarr created, and it never
//! cancels Lidarr's own queue: `cancel-download` only forgets the plugin's
//! state, so an administrator's download keeps running.
//!
//! The import root must expose the files of Lidarr's root folder, and it must
//! use mode `link` so the server imports them by hardlink and leaves Lidarr's
//! files untouched.
//!
//! The pure logic sits in separate modules ([`config`], [`clock`],
//! [`download`], [`lidarr`], [`quality`], [`search`], [`tokens`], [`url`]); the
//! binding-coupled glue that drives it stays in this file.
//!
//! # Tests
//!
//! The pure logic is unit-tested on the host target:
//!
//! ```text
//! cargo test
//! ```
//!
//! Rustdoc examples in those modules are illustrative `text` blocks rather than
//! doctests: the crate is a `cdylib` for `wasm32-wasip2`, so there is no host
//! library target for a doctest to link against. Every example appears as an
//! assertion in the unit tests of the module that carries it.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use hearthsound_plugin::acquisition_provider::exports::hearthsound::plugin::acquisition::{
    Capabilities, DownloadState as DownloadStateWit, Granularity, Guest as AcquisitionGuest,
    OutputFile as WitOutputFile, Progress, Query, SearchState as SearchStateWit,
};
use hearthsound_plugin::acquisition_provider::exports::hearthsound::plugin::lifecycle::Guest as LifecycleGuest;
use hearthsound_plugin::host;
use hearthsound_plugin::kv;
use hearthsound_plugin::types::ProviderError;

mod api;
mod clock;
mod config;
mod download;
#[cfg(test)]
mod fixtures;
mod lidarr;
mod quality;
mod random;
mod search;
mod tokens;
mod url;

use api::LidarrApi;

/// Prefix of the host-kv keys that hold one search's candidates.
const SEARCH_KEY_PREFIX: &str = "search:";

/// Prefix of the host-kv keys that hold one download's state.
const DOWNLOAD_KEY_PREFIX: &str = "download:";

/// How long a search or download entry stays readable, in milliseconds.
///
/// An hour is far longer than a lookup or an acquisition can take, and it
/// bounds how long the state of a search the server stopped polling occupies
/// the store.
const STATE_TTL_MS: u64 = 3_600_000;

/// The plugin implementation.
struct Lidarr;

/// Validates a server-scoped configuration document.
///
/// Runs the schema-level checks of [`config::check`] and, when they pass, asks
/// the configured instance for its system status, because a wrong URL or API
/// key is a problem an administrator has to fix before the plugin can acquire
/// anything.
///
/// # Parameters
///
/// - `server_config`: the JSON document the administrator entered, with the
///   schema defaults the admin UI filled in.
///
/// # Returns
///
/// `Ok(())` when the document is usable and the instance answers.
///
/// # Errors
///
/// Returns one message per problem: a missing or malformed field, or an
/// instance that cannot be reached or authenticated with.
pub fn validate_config(server_config: &str) -> Result<(), Vec<String>> {
    let problems = config::check(server_config);
    if !problems.is_empty() {
        return Err(problems);
    }

    let config = config::Config::from_json(server_config).map_err(|message| vec![message])?;

    match api::Client::new(&config).health() {
        Ok(version) => {
            hearthsound_plugin::info!("Lidarr {version} at {}", config.base_url());
            Ok(())
        }
        Err(error) => Err(vec![format!(
            "Lidarr at {}: {}",
            config.base_url(),
            api::describe(&error)
        )]),
    }
}

/// Probes the configured Lidarr instance.
///
/// # Returns
///
/// The Lidarr version the instance reports.
///
/// # Errors
///
/// Returns [`ProviderError::InvalidConfig`] when this instance's configuration
/// does not pass [`config::check`], and the mapped error of a failed request.
pub fn health() -> Result<String, ProviderError> {
    api::Client::new(&configured()?).health()
}

/// Implements the lifecycle interface.
impl LifecycleGuest for Lidarr {
    fn validate_config(server_config: String) -> Result<(), Vec<String>> {
        validate_config(&server_config)
    }

    fn health() -> Result<String, ProviderError> {
        health()
    }
}

/// Implements the acquisition interface.
impl AcquisitionGuest for Lidarr {
    fn get_capabilities() -> Capabilities {
        Capabilities {
            granularities: vec![Granularity::Album],
            progressive: false,
        }
    }

    fn start_search(query: Query) -> Result<String, ProviderError> {
        let config = configured()?;

        if query.granularity != Granularity::Album {
            return Err(ProviderError::Internal(
                "this plugin acquires albums only".to_string(),
            ));
        }

        if query.tracks.is_empty() {
            return Err(ProviderError::Internal(
                "the query carries no wanted track".to_string(),
            ));
        }

        let terms = search::terms(&query)?;
        let client = api::Client::new(&config);
        let profile = client.quality_profile(config.quality_profile_id)?;
        let mapping = quality::mapping(&profile);
        let candidates = search::find(&query, &terms, &mapping, |term| client.lookup_albums(term))?;

        hearthsound_plugin::info!(
            "Lidarr lookup tried at most {} term(s) and returned {} candidate(s)",
            terms.len(),
            candidates.len()
        );

        let key = random::key();
        let stored = search::StoredSearch { candidates };
        kv::set_json(&search_key(&key), &stored, Some(STATE_TTL_MS))?;

        tokens::encode(&search::SearchToken { key })
    }

    fn poll_search(search_token: String) -> Result<SearchStateWit, ProviderError> {
        let token: search::SearchToken = tokens::decode(&search_token)?;
        let stored: search::StoredSearch =
            kv::get_json(&search_key(&token.key))?.ok_or(ProviderError::NotFound)?;

        let mut candidates = Vec::with_capacity(stored.candidates.len());
        for candidate in &stored.candidates {
            let encoded = tokens::encode(candidate)?;
            candidates.push(search::view(candidate, encoded));
        }

        Ok(SearchStateWit::Complete(candidates))
    }

    fn cancel_search(search_token: String) {
        let Ok(token) = tokens::decode::<search::SearchToken>(&search_token) else {
            hearthsound_plugin::warn!("cannot cancel a search from an unreadable token");

            return;
        };

        host::kv::delete(&search_key(&token.key));
    }

    fn start_download(candidate_token: String, files: Vec<u32>) -> Result<String, ProviderError> {
        let config = configured()?;
        let candidate: search::CandidateToken = tokens::decode(&candidate_token)?;
        let selected = download::select(&candidate.files, &files)?;

        if selected.is_empty() {
            return Err(ProviderError::Internal(
                "the candidate offers no file to download".to_string(),
            ));
        }

        let client = api::Client::new(&config);
        let state = download::prepare(&client, &clock::SystemClock, &config, &candidate)?;
        let token = download::DownloadToken {
            key: random::key(),
            album_foreign_id: candidate.album_foreign_id,
            album_title: candidate.album_title,
            files: selected,
        };

        hearthsound_plugin::info!(
            "asked Lidarr for `{}` ({} track(s))",
            token.album_title,
            token.files.len()
        );

        kv::set_json(&download_key(&token.key), &state, Some(STATE_TTL_MS))?;

        tokens::encode(&token)
    }

    fn poll_download(download_token: String) -> Result<DownloadStateWit, ProviderError> {
        let token: download::DownloadToken = tokens::decode(&download_token)?;
        let config = configured()?;
        let mut state: download::DownloadState =
            kv::get_json(&download_key(&token.key))?.ok_or(ProviderError::NotFound)?;
        let client = api::Client::new(&config);
        let outcome = download::advance(
            &client,
            &clock::SystemClock,
            &config,
            &HostPresence,
            &token,
            &mut state,
        )?;

        kv::set_json(&download_key(&token.key), &state, Some(STATE_TTL_MS))?;

        Ok(match outcome {
            download::Advance::Queued(length) => DownloadStateWit::Queued(length),
            download::Advance::Running { bytes, total_bytes } => {
                DownloadStateWit::Running(Progress {
                    bytes,
                    total_bytes,
                    speed_bps: None,
                })
            }
            download::Advance::Complete(outputs) => DownloadStateWit::Complete(
                outputs
                    .into_iter()
                    .map(|output| WitOutputFile {
                        root: config.import_root.clone(),
                        path: output.path,
                        track_index: Some(output.track_index),
                    })
                    .collect(),
            ),
            download::Advance::Failed(message) => DownloadStateWit::Failed(message),
        })
    }

    fn cancel_download(download_token: String) {
        let Ok(token) = tokens::decode::<download::DownloadToken>(&download_token) else {
            hearthsound_plugin::warn!("cannot cancel a download from an unreadable token");

            return;
        };

        // Lidarr's queue and files belong to the administrator, so cancelling
        // only forgets the plugin's own state.
        host::kv::delete(&download_key(&token.key));
    }
}

hearthsound_plugin::export_acquisition_provider!(Lidarr);

/// Reports whether a file is present inside an import root.
struct HostPresence;

impl download::Presence for HostPresence {
    fn present(&self, root: &str, path: &str) -> bool {
        host::files::stat(root, path).is_some_and(|stat| !stat.is_dir && stat.size > 0)
    }
}

/// Reads this instance's configuration through the host.
///
/// # Returns
///
/// The typed configuration of the instance the call belongs to.
///
/// # Errors
///
/// Returns [`ProviderError::InvalidConfig`] when the stored document does not
/// deserialize.
fn configured() -> Result<config::Config, ProviderError> {
    let json = host::config::server_config();

    config::Config::from_json(&json).map_err(ProviderError::InvalidConfig)
}

/// Returns the host-kv key that holds one search's candidates.
///
/// # Parameters
///
/// - `key`: the search token's key.
///
/// # Returns
///
/// The store key, prefixed so a search entry cannot collide with another part
/// of the plugin's state.
fn search_key(key: &str) -> String {
    format!("{SEARCH_KEY_PREFIX}{key}")
}

/// Returns the host-kv key that holds one download's state.
///
/// # Parameters
///
/// - `key`: the download token's key.
///
/// # Returns
///
/// The store key, prefixed so a download entry cannot collide with another part
/// of the plugin's state.
fn download_key(key: &str) -> String {
    format!("{DOWNLOAD_KEY_PREFIX}{key}")
}
