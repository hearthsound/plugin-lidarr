//! Growing an album request into a Lidarr download, and watching it finish.
//!
//! Lidarr owns the search, the download client and the import. The plugin's job
//! is to make the album known to Lidarr - adding its artist when the library
//! does not have it - monitor the album, ask for its search, and then report
//! what Lidarr imported. Completion follows Lidarr's own album state: once the
//! album is in the library, the plugin reads its tracks and reports every
//! listed wanted track that Lidarr holds and that is present inside the
//! configured import root. A wanted track Lidarr's album does not hold - a
//! standard edition offered for a deluxe request, for example - is not waited
//! for; the server fans it out as a track acquisition. An album Lidarr reports
//! fully imported with nothing in its queue also finishes, so a track the
//! import root cannot show does not hold the acquisition until it times out.
//!
//! Progress mirrors what Lidarr can observe. A poll answers `running` only
//! while a queue item actively downloads and its byte count advanced since the
//! previous poll; Lidarr's search and grab, a held or paused item, and its
//! import phase answer `queued`, because none of them moves bytes the server
//! could mistake for a stall.
//!
//! The sequence is expressed against [`LidarrApi`] and [`Presence`], so the
//! whole flow is unit-tested against fixtures without a Lidarr instance.
//!
//! The plugin never deletes or modifies a file Lidarr created, and it never
//! deletes an artist or an album from Lidarr's library. Cancelling a download
//! undoes what the plugin itself started: it removes the queue items the album
//! has - with the download client's copy, so the transfer stops - and clears
//! the monitored flag of the album when the plugin set it, leaving the files
//! Lidarr imported and every album the administrator follows alone.

use hearthsound_plugin::types::ProviderError;

use crate::api::LidarrApi;
use crate::clock::{self, Clock};
use crate::config;
use crate::lidarr;
use crate::search::{CandidateToken, WantedFile};

/// The token `start-download` returns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadToken {
    /// The download's random key, which names its store entry.
    pub key: String,
    /// The album's release-group MusicBrainz id.
    pub album_foreign_id: String,
    /// The album title Lidarr reported.
    pub album_title: String,
    /// The wanted tracks the download is for.
    pub files: Vec<WantedFile>,
}

/// The mutable state of one download, kept in the instance's store.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadState {
    /// Lidarr's id of the album once it is in the library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<u32>,
    /// Lidarr's id of the album's artist.
    pub artist_id: u32,
    /// When the album search was issued, in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// The bytes the album's queue reported at the previous poll.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_bytes: Option<u64>,
    /// Whether the plugin set the album's monitored flag, which a cancel
    /// clears again. An album the administrator already monitored stays
    /// monitored.
    #[serde(default)]
    pub monitored_album: bool,
}

/// One file the plugin reports to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The path relative to the configured import root.
    pub path: String,
    /// The wanted track's index.
    pub track_index: u32,
}

/// What one poll of a download found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advance {
    /// Lidarr has not queued anything yet; the number is how many queue items
    /// the album has, when that is known.
    Queued(Option<u32>),
    /// Lidarr is downloading; the sums are over the album's queue items.
    Running {
        /// Bytes downloaded so far.
        bytes: u64,
        /// Total bytes to download, when the queue reports a size.
        total_bytes: Option<u64>,
    },
    /// Every wanted track is imported.
    Complete(Vec<Output>),
    /// The acquisition failed, with the reason for the log.
    Failed(String),
}

/// Reports whether a file is present at a path.
///
/// The real implementation asks the host about the import root; the tests
/// answer from a set.
pub(crate) trait Presence {
    /// Returns whether `path` in `root` is a non-empty file.
    ///
    /// # Parameters
    ///
    /// - `root`: the import-root id.
    /// - `path`: the path relative to the root.
    fn present(&self, root: &str, path: &str) -> bool;
}

/// Selects the files a download asks for.
///
/// # Parameters
///
/// - `files`: the files the candidate offers.
/// - `selected`: the indices the server asked for, in the order it asked for
///   them; an empty list means every file of the candidate.
///
/// # Returns
///
/// The selected files, in the requested order.
///
/// # Errors
///
/// Returns [`ProviderError::Internal`] when an index does not name a file of
/// the candidate, which means the server and the plugin disagree about the
/// candidate.
///
/// # Example
///
/// ```text
/// select(files of 2, [1]) -> [files[1]]
/// ```
pub(crate) fn select(
    files: &[WantedFile],
    selected: &[u32],
) -> Result<Vec<WantedFile>, ProviderError> {
    if selected.is_empty() {
        return Ok(files.to_vec());
    }

    selected
        .iter()
        .map(|index| {
            files.get(*index as usize).cloned().ok_or_else(|| {
                ProviderError::Internal(format!(
                    "the candidate has no file {index}; it offers {}",
                    files.len()
                ))
            })
        })
        .collect()
}

/// Makes an album known to Lidarr and starts its search when possible.
///
/// # Parameters
///
/// - `api`: the Lidarr instance.
/// - `clock`: the source of the current instant.
/// - `config`: the instance configuration, which names the profiles, the root
///   folder and the import root.
/// - `candidate`: the candidate the server chose.
///
/// # Returns
///
/// The download state. `album_id` stays empty when Lidarr has not listed the
/// album yet; the next poll looks again.
///
/// # Errors
///
/// Returns the mapped error of a failed request, and
/// [`ProviderError::InvalidConfig`] when the configuration cannot name an
/// artist.
pub(crate) fn prepare(
    api: &dyn LidarrApi,
    clock: &dyn Clock,
    config: &config::Config,
    candidate: &CandidateToken,
) -> Result<DownloadState, ProviderError> {
    let artist_id = ensure_artist(api, config, candidate)?;
    let now = clock.now();
    let mut state = DownloadState {
        album_id: None,
        artist_id,
        started_at: now,
        last_bytes: None,
        monitored_album: false,
    };

    if let Some(album_id) = find_album(api, &candidate.album_foreign_id)? {
        start(api, album_id, &mut state)?;
        state.album_id = Some(album_id);
    }

    Ok(state)
}

/// Polls one download and advances its state.
///
/// # Parameters
///
/// - `api`: the Lidarr instance.
/// - `clock`: the source of the current instant.
/// - `config`: the instance configuration.
/// - `presence`: the check for imported files.
/// - `token`: the download's token.
/// - `state`: the download's state, updated in place.
///
/// # Returns
///
/// What the poll found. The download completes when every album track that
/// maps to a listed wanted track has a file present inside the import root, or
/// when Lidarr reports every track of the album imported and its queue holds
/// nothing for it; the outputs are the files found, each with the wanted
/// track's index.
///
/// Progress answers `running` with the summed byte count only while a queue
/// item actively downloads and that count advanced since the previous poll.
/// Lidarr's search and grab, held queue items and its import phase answer
/// `queued`.
///
/// # Errors
///
/// Returns the mapped error of a failed request. A Lidarr error that predates
/// the download is ignored by [`lidarr::has_failed_download`].
pub(crate) fn advance(
    api: &dyn LidarrApi,
    clock: &dyn Clock,
    config: &config::Config,
    presence: &dyn Presence,
    token: &DownloadToken,
    state: &mut DownloadState,
) -> Result<Advance, ProviderError> {
    let now = clock.now();

    let album_id = match state.album_id {
        Some(album_id) => album_id,
        None => {
            let Some(album_id) = find_album(api, &token.album_foreign_id)? else {
                return Ok(waiting(config, state.started_at, now, "list the album"));
            };

            start(api, album_id, state)?;
            state.album_id = Some(album_id);
            state.started_at = now.or(state.started_at);
            album_id
        }
    };

    let tracks = api.tracks(album_id)?;
    let track_files = api.track_files(album_id)?;
    let matches = match_tracks(&token.files, &tracks);
    let root = config.root_folder();
    let mut mapped = 0usize;
    let mut present = 0usize;
    let mut outputs = Vec::new();

    for (position, wanted) in token.files.iter().enumerate() {
        let Some(track_index) = matches.get(position).copied().flatten() else {
            continue;
        };
        mapped += 1;

        let Some(file_index) = track_file(&track_files, &tracks[track_index], wanted) else {
            continue;
        };
        let Some(path) = track_files[file_index].path.as_deref() else {
            continue;
        };
        let Some(relative) = relative_path(&root, path) else {
            continue;
        };

        if presence.present(&config.import_root, &relative) {
            present += 1;
            outputs.push(Output {
                path: relative,
                track_index: wanted.index,
            });
        }
    }

    if mapped > 0 && present == mapped {
        return Ok(Advance::Complete(outputs));
    }

    let queue = api.queue(album_id)?;
    let imported = !tracks.is_empty() && tracks.iter().all(|track| track.has_file);

    if imported && queue.is_empty() {
        return Ok(Advance::Complete(outputs));
    }

    if api.failed_download(album_id, state.started_at)? {
        return Ok(Advance::Failed(format!(
            "Lidarr reported a failed download for `{}`",
            token.album_title
        )));
    }

    if !queue.is_empty() {
        let total: f64 = queue.iter().map(|item| item.size).sum();
        let left: f64 = queue.iter().map(|item| item.sizeleft).sum();
        let bytes = (total - left).max(0.0) as u64;
        let total_bytes = (total > 0.0).then_some(total as u64);
        let importing = left <= 0.0 || queue.iter().any(is_importing);
        let advanced = state.last_bytes.is_none_or(|last| bytes > last);

        state.last_bytes = Some(bytes);

        if !importing && bytes > 0 && advanced && queue.iter().any(is_downloading) {
            return Ok(Advance::Running { bytes, total_bytes });
        }

        return Ok(Advance::Queued(Some(queue.len() as u32)));
    }

    if present == 0 {
        return Ok(waiting(config, state.started_at, now, "start a download"));
    }

    Ok(Advance::Queued(None))
}

/// Returns the download outcome when Lidarr has nothing to report yet.
///
/// # Parameters
///
/// - `config`: the instance configuration, which supplies the wait.
/// - `started_at`: when the search was issued.
/// - `now`: the current instant.
/// - `subject`: the verb phrase the failure message completes.
///
/// # Returns
///
/// A failure once the wait has passed, and a queued report before that.
fn waiting(
    config: &config::Config,
    started_at: Option<i64>,
    now: Option<i64>,
    subject: &str,
) -> Advance {
    if clock::timed_out(started_at, now, config.search_wait_minutes) {
        Advance::Failed(format!(
            "Lidarr did not {subject} within {} minutes",
            config.search_wait_minutes
        ))
    } else {
        Advance::Queued(None)
    }
}

/// Ensures Lidarr holds the candidate's artist and returns its id.
///
/// # Parameters
///
/// - `api`: the Lidarr instance.
/// - `config`: the instance configuration.
/// - `candidate`: the candidate, which names the artist.
///
/// # Returns
///
/// Lidarr's id of the artist.
///
/// # Errors
///
/// Returns the mapped error of a failed request.
fn ensure_artist(
    api: &dyn LidarrApi,
    config: &config::Config,
    candidate: &CandidateToken,
) -> Result<u32, ProviderError> {
    let artists = api.artists_by_mbid(&candidate.artist_foreign_id)?;
    if let Some(artist) = artists
        .into_iter()
        .find(|artist| artist.foreign_artist_id.as_deref() == Some(&candidate.artist_foreign_id))
    {
        return Ok(artist.id);
    }

    let request = lidarr::AddArtist {
        artist_name: candidate.artist_name.clone(),
        foreign_artist_id: candidate.artist_foreign_id.clone(),
        quality_profile_id: config.quality_profile_id,
        metadata_profile_id: config.metadata_profile_id,
        root_folder_path: config.root_folder(),
        monitored: true,
        add_options: lidarr::AddArtistOptions {
            monitor: "none".to_string(),
            albums_to_monitor: vec![candidate.album_foreign_id.clone()],
            search_for_missing_albums: false,
        },
    };

    Ok(api.add_artist(&request)?.id)
}

/// Finds Lidarr's id of an album that is in the library.
///
/// # Parameters
///
/// - `api`: the Lidarr instance.
/// - `foreign_id`: the album's release-group MusicBrainz id.
///
/// # Returns
///
/// The album's Lidarr id, and `None` while Lidarr has not listed the album,
/// which is a normal step of adding an artist.
///
/// # Errors
///
/// Returns the mapped error of a failed request.
fn find_album(api: &dyn LidarrApi, foreign_id: &str) -> Result<Option<u32>, ProviderError> {
    Ok(api
        .albums_by_foreign_id(foreign_id)?
        .into_iter()
        .find(|album| album.id > 0 && album.foreign_album_id.as_deref() == Some(foreign_id))
        .map(|album| album.id))
}

/// Monitors one album, records that the plugin did so, and asks Lidarr to
/// search for it.
///
/// # Parameters
///
/// - `api`: the Lidarr instance.
/// - `album_id`: Lidarr's id of the album.
/// - `state`: the download's state, which records that the plugin set the
///   album's monitored flag so a cancel can clear it again.
///
/// # Errors
///
/// Returns the mapped error of a failed request.
fn start(
    api: &dyn LidarrApi,
    album_id: u32,
    state: &mut DownloadState,
) -> Result<(), ProviderError> {
    api.monitor_album(album_id)?;
    state.monitored_album = true;

    api.search_album(album_id)
}

/// Cancels one download in Lidarr.
///
/// Undoes what the plugin started and nothing else: every queue item of the
/// album is removed with the download client's copy, which stops a transfer in
/// flight, and the album's monitored flag is cleared when the plugin set it.
/// The album, the artist and the files Lidarr imported stay.
///
/// # Parameters
///
/// - `api`: the Lidarr instance.
/// - `state`: the download's state, or `None` when the store no longer holds
///   one, which makes the cancel a no-op.
/// - `token`: the download's token, which names the album when the state was
///   stored before Lidarr listed it.
///
/// # Returns
///
/// `Ok(())` once Lidarr has been asked to drop the queue items and the
/// monitored flag, including when there is nothing to undo.
///
/// # Errors
///
/// Returns the first error the instance reported. Every other step is still
/// attempted, because a queue item that vanished (a `404`) must not leave the
/// album monitored.
pub(crate) fn cancel(
    api: &dyn LidarrApi,
    state: Option<&DownloadState>,
    token: &DownloadToken,
) -> Result<(), ProviderError> {
    let Some(state) = state else {
        return Ok(());
    };

    let album_id = match state.album_id {
        Some(album_id) => Some(album_id),
        None => find_album(api, &token.album_foreign_id)?,
    };
    let Some(album_id) = album_id else {
        return Ok(());
    };

    let mut failure = None;
    let items = match api.queue(album_id) {
        Ok(items) => items,
        Err(error) => {
            failure = Some(error);

            Vec::new()
        }
    };

    for item in items {
        if let Err(error) = api.remove_queue_item(item.id, true) {
            failure = failure.or(Some(error));
        }
    }

    if state.monitored_album
        && let Err(error) = api.unmonitor_album(album_id)
    {
        failure = failure.or(Some(error));
    }

    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Queue statuses in which Lidarr holds a download back instead of moving
/// bytes.
const HELD_STATES: [&str; 4] = ["queued", "paused", "delay", "downloadClientUnavailable"];

/// Tracked download states in which Lidarr works on the downloaded files.
const IMPORT_STATES: [&str; 2] = ["importPending", "importing"];

/// Returns whether a queue item is actively downloading.
///
/// Lidarr reports the download client's status and its own tracked state; an
/// item that is held, paused or being imported is not moving bytes even when
/// one of the two fields still says `downloading`.
///
/// # Parameters
///
/// - `item`: the queue item.
///
/// # Returns
///
/// `true` when one field reports `downloading` and neither field reports a held
/// or import state.
fn is_downloading(item: &lidarr::QueueItem) -> bool {
    let states = [
        item.status.as_deref(),
        item.tracked_download_state.as_deref(),
    ];

    states
        .iter()
        .flatten()
        .any(|state| state.eq_ignore_ascii_case("downloading"))
        && !states
            .iter()
            .flatten()
            .any(|state| is_held(state) || is_importing_state(state))
}

/// Returns whether a queue item is being imported.
///
/// # Parameters
///
/// - `item`: the queue item.
///
/// # Returns
///
/// `true` when Lidarr's tracked state reports `importPending` or `importing`.
fn is_importing(item: &lidarr::QueueItem) -> bool {
    [
        item.status.as_deref(),
        item.tracked_download_state.as_deref(),
    ]
    .iter()
    .flatten()
    .any(|state| is_importing_state(state))
}

/// Reports whether one queue state holds a download back.
///
/// # Parameters
///
/// - `state`: a queue status or tracked download state.
///
/// # Returns
///
/// `true` for the statuses in [`HELD_STATES`].
fn is_held(state: &str) -> bool {
    HELD_STATES
        .iter()
        .any(|held| state.eq_ignore_ascii_case(held))
}

/// Reports whether one queue state is an import state.
///
/// # Parameters
///
/// - `state`: a queue status or tracked download state.
///
/// # Returns
///
/// `true` for the states in [`IMPORT_STATES`].
fn is_importing_state(state: &str) -> bool {
    IMPORT_STATES
        .iter()
        .any(|import| state.eq_ignore_ascii_case(import))
}

/// Matches the tracks of Lidarr's album to the listed wanted tracks.
///
/// Lidarr knows the album's real tracklist, which may hold fewer tracks than
/// the query wanted: a standard edition offered for a deluxe request holds 13
/// of 16. The match uses normalized titles, and pairs each wanted track with at
/// most one album track.
///
/// # Parameters
///
/// - `wanted`: the listed wanted tracks.
/// - `tracks`: the tracks of the album in Lidarr's library.
///
/// # Returns
///
/// For each listed wanted track, the index of its album track, or `None` when
/// Lidarr's album does not hold it.
///
/// # Example
///
/// ```text
/// match_tracks([Kid A], [track "Kid A"]) -> [Some(0)]
/// ```
pub(crate) fn match_tracks(wanted: &[WantedFile], tracks: &[lidarr::Track]) -> Vec<Option<usize>> {
    let mut matches = vec![None; wanted.len()];
    let mut claimed = vec![false; tracks.len()];

    for (position, wanted) in wanted.iter().enumerate() {
        let title = normalize(&wanted.title);

        if title.is_empty() {
            continue;
        }

        let found = tracks.iter().enumerate().find(|(index, track)| {
            !claimed[*index]
                && track
                    .title
                    .as_deref()
                    .is_some_and(|track_title| normalize(track_title) == title)
        });

        if let Some((index, _)) = found {
            claimed[index] = true;
            matches[position] = Some(index);
        }
    }

    matches
}

/// Finds the track file of one album track.
///
/// # Parameters
///
/// - `files`: the track files Lidarr reported for the album.
/// - `track`: the album track.
/// - `wanted`: the listed wanted track the album track holds.
///
/// # Returns
///
/// The index of the file in `files`: the file the track names, or the file
/// whose tags or path name the wanted track when the track names none.
fn track_file(
    files: &[lidarr::TrackFile],
    track: &lidarr::Track,
    wanted: &WantedFile,
) -> Option<usize> {
    if let Some(id) = track.track_file_id
        && let Some(index) = files.iter().position(|file| file.id == id)
    {
        return Some(index);
    }

    match_files(std::slice::from_ref(wanted), files)
        .first()
        .copied()
        .flatten()
}

/// Matches Lidarr's track files to the wanted tracks.
///
/// Lidarr's track file records do not carry the release-group track index, so
/// the match uses the tags Lidarr parsed: first an equal title, then a title
/// found in the file name. When both sides have the same size, the remaining
/// entries pair up in order, which covers files whose tags Lidarr could not
/// read. [`advance`] uses this to find the file of an album track that does not
/// name its file itself.
///
/// # Parameters
///
/// - `wanted`: the wanted tracks, in query order.
/// - `files`: the track files Lidarr reported.
///
/// # Returns
///
/// For each wanted track, the index of its file, or `None` when no file could
/// be matched.
///
/// # Example
///
/// ```text
/// match_files([Kid A], [file titled "Kid A"]) -> [Some(0)]
/// ```
pub(crate) fn match_files(
    wanted: &[WantedFile],
    files: &[lidarr::TrackFile],
) -> Vec<Option<usize>> {
    let mut matches = vec![None; wanted.len()];
    let mut claimed = vec![false; files.len()];

    for (position, track) in wanted.iter().enumerate() {
        let title = normalize(&track.title);
        if title.is_empty() {
            continue;
        }

        let found = files.iter().enumerate().find(|(index, file)| {
            !claimed[*index]
                && file
                    .audio_tags
                    .as_ref()
                    .and_then(|tags| tags.title.as_deref())
                    .is_some_and(|file_title| normalize(file_title) == title)
        });

        if let Some((index, _)) = found {
            claimed[index] = true;
            matches[position] = Some(index);
        }
    }

    for (position, track) in wanted.iter().enumerate() {
        if matches[position].is_some() {
            continue;
        }

        let title = normalize(&track.title);
        if title.is_empty() {
            continue;
        }

        let found = files.iter().enumerate().find(|(index, file)| {
            !claimed[*index]
                && file
                    .path
                    .as_deref()
                    .map(basename)
                    .is_some_and(|name| normalize(name).contains(&title))
        });

        if let Some((index, _)) = found {
            claimed[index] = true;
            matches[position] = Some(index);
        }
    }

    if files.len() == wanted.len() {
        for (position, matched) in matches.iter_mut().enumerate() {
            if matched.is_none() && !claimed[position] {
                claimed[position] = true;
                *matched = Some(position);
            }
        }
    }

    matches
}

/// Returns a track file's path relative to Lidarr's root folder.
///
/// # Parameters
///
/// - `root`: the root folder path as Lidarr sees it.
/// - `path`: a track file path as Lidarr reports it.
///
/// # Returns
///
/// The path below the root, which is the path inside the import root. `None`
/// when the file is outside the root or the relative path could climb out of
/// it.
///
/// # Example
///
/// ```text
/// relative_path("/music", "/music/Artist/Album/01 Song.flac") -> Some("Artist/Album/01 Song.flac")
/// ```
pub(crate) fn relative_path(root: &str, path: &str) -> Option<String> {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return None;
    }

    let relative = path.strip_prefix(root)?.strip_prefix('/')?;
    if relative.is_empty() || relative.split('/').any(|part| part == "..") {
        return None;
    }

    Some(relative.to_string())
}

/// Normalizes a title for comparison.
///
/// # Parameters
///
/// - `value`: the title.
///
/// # Returns
///
/// The lowercased title with every non-alphanumeric run replaced by one space.
fn normalize(value: &str) -> String {
    value
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns the last component of a path.
///
/// # Parameters
///
/// - `path`: the path.
///
/// # Returns
///
/// The file name after the last separator of either style.
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use crate::quality;
    use hearthsound_plugin::acquisition_provider::exports::hearthsound::plugin::acquisition::{
        Granularity, Query, WantedTrack,
    };
    use std::cell::RefCell;
    use std::collections::HashSet;

    const ALBUM: &str = "f0f0f0f0-0000-0000-0000-000000000001";
    const ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

    struct Fake {
        artists: Vec<lidarr::Artist>,
        albums: Vec<lidarr::Album>,
        tracks: Vec<lidarr::Track>,
        files: Vec<lidarr::TrackFile>,
        queue: Vec<lidarr::QueueItem>,
        failed: bool,
        fail_queue_removal: bool,
        fail_unmonitor: bool,
        calls: RefCell<Vec<String>>,
        added: RefCell<Option<lidarr::AddArtist>>,
        removed: RefCell<Vec<(u32, bool)>>,
        unmonitored: RefCell<Vec<u32>>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                artists: Vec::new(),
                albums: vec![lidarr::Album {
                    id: 21,
                    title: Some("Kid A".to_string()),
                    foreign_album_id: Some(ALBUM.to_string()),
                    artist_id: 9,
                    artist: None,
                    releases: Vec::new(),
                }],
                tracks: fixtures::json("tracks.json"),
                files: Vec::new(),
                queue: Vec::new(),
                failed: false,
                fail_queue_removal: false,
                fail_unmonitor: false,
                calls: RefCell::new(Vec::new()),
                added: RefCell::new(None),
                removed: RefCell::new(Vec::new()),
                unmonitored: RefCell::new(Vec::new()),
            }
        }

        fn call(&self, name: &str) {
            self.calls.borrow_mut().push(name.to_string());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn removed(&self) -> Vec<(u32, bool)> {
            self.removed.borrow().clone()
        }

        fn unmonitored(&self) -> Vec<u32> {
            self.unmonitored.borrow().clone()
        }
    }

    impl LidarrApi for Fake {
        fn lookup_albums(&self, _term: &str) -> Result<Vec<lidarr::Album>, ProviderError> {
            self.call("lookup_albums");

            Ok(Vec::new())
        }

        fn quality_profile(&self, _id: u32) -> Result<lidarr::QualityProfile, ProviderError> {
            self.call("quality_profile");

            Ok(fixtures::json("quality_profile.json"))
        }

        fn artists_by_mbid(&self, _mbid: &str) -> Result<Vec<lidarr::Artist>, ProviderError> {
            self.call("artists_by_mbid");

            Ok(self.artists.clone())
        }

        fn add_artist(&self, artist: &lidarr::AddArtist) -> Result<lidarr::Artist, ProviderError> {
            self.call("add_artist");
            *self.added.borrow_mut() = Some(artist.clone());

            Ok(lidarr::Artist {
                id: 9,
                artist_name: Some(artist.artist_name.clone()),
                foreign_artist_id: Some(artist.foreign_artist_id.clone()),
                root_folder_path: Some(artist.root_folder_path.clone()),
            })
        }

        fn albums_by_foreign_id(
            &self,
            _foreign_id: &str,
        ) -> Result<Vec<lidarr::Album>, ProviderError> {
            self.call("albums_by_foreign_id");

            Ok(self.albums.clone())
        }

        fn monitor_album(&self, _album_id: u32) -> Result<(), ProviderError> {
            self.call("monitor_album");

            Ok(())
        }

        fn search_album(&self, _album_id: u32) -> Result<(), ProviderError> {
            self.call("search_album");

            Ok(())
        }

        fn queue(&self, _album_id: u32) -> Result<Vec<lidarr::QueueItem>, ProviderError> {
            self.call("queue");

            Ok(self.queue.clone())
        }

        fn remove_queue_item(
            &self,
            id: u32,
            remove_from_client: bool,
        ) -> Result<(), ProviderError> {
            self.call("remove_queue_item");
            self.removed.borrow_mut().push((id, remove_from_client));

            if self.fail_queue_removal {
                return Err(ProviderError::NotFound);
            }

            Ok(())
        }

        fn unmonitor_album(&self, album_id: u32) -> Result<(), ProviderError> {
            self.call("unmonitor_album");
            self.unmonitored.borrow_mut().push(album_id);

            if self.fail_unmonitor {
                return Err(ProviderError::Unavailable("queue busy".to_string()));
            }

            Ok(())
        }

        fn tracks(&self, _album_id: u32) -> Result<Vec<lidarr::Track>, ProviderError> {
            self.call("tracks");

            Ok(self.tracks.clone())
        }

        fn track_files(&self, _album_id: u32) -> Result<Vec<lidarr::TrackFile>, ProviderError> {
            self.call("track_files");

            Ok(self.files.clone())
        }

        fn failed_download(
            &self,
            _album_id: u32,
            _since: Option<i64>,
        ) -> Result<bool, ProviderError> {
            self.call("failed_download");

            Ok(self.failed)
        }
    }

    struct FixedClock(Option<i64>);

    impl Clock for FixedClock {
        fn now(&self) -> Option<i64> {
            self.0
        }
    }

    struct Files(HashSet<String>);

    impl Files {
        fn with(paths: &[&str]) -> Self {
            Self(paths.iter().map(|path| (*path).to_string()).collect())
        }
    }

    impl Presence for Files {
        fn present(&self, root: &str, path: &str) -> bool {
            root == "lidarr" && self.0.contains(path)
        }
    }

    fn config() -> config::Config {
        config::Config::from_json(
            r#"{
                "url": "http://lidarr:8686",
                "apiKey": "secret",
                "importRoot": "lidarr",
                "rootFolderPath": "/music/",
                "qualityProfileId": 4,
                "metadataProfileId": 2,
                "searchWaitMinutes": 30
            }"#,
        )
        .unwrap()
    }

    fn candidate() -> CandidateToken {
        let albums: Vec<lidarr::Album> = fixtures::json("lookup.json");
        let query = Query {
            granularity: Granularity::Album,
            album: Some("Kid A".to_string()),
            album_artist: Some("Radiohead".to_string()),
            year: Some(2000),
            tracks: vec![
                WantedTrack {
                    title: "Everything In Its Right Place".to_string(),
                    artists: vec!["Radiohead".to_string()],
                    track_number: Some(1),
                    disc_number: Some(1),
                    duration_ms: Some(240_000),
                    isrc: None,
                    ids: Vec::new(),
                },
                WantedTrack {
                    title: "Kid A".to_string(),
                    artists: vec!["Radiohead".to_string()],
                    track_number: Some(2),
                    disc_number: Some(1),
                    duration_ms: Some(200_000),
                    isrc: None,
                    ids: Vec::new(),
                },
            ],
            preferred_formats: vec!["flac".to_string()],
        };

        crate::search::candidates(
            &query,
            &albums,
            &quality::Mapping {
                format: "flac".to_string(),
                bitrate_kbps: None,
            },
        )
        .remove(0)
    }

    fn token() -> DownloadToken {
        DownloadToken {
            key: "key".to_string(),
            album_foreign_id: ALBUM.to_string(),
            album_title: "Kid A".to_string(),
            files: candidate().files,
        }
    }

    fn state(album_id: Option<u32>, started_at: Option<i64>) -> DownloadState {
        DownloadState {
            album_id,
            artist_id: 9,
            started_at,
            last_bytes: None,
            monitored_album: false,
        }
    }

    fn monitored_state() -> DownloadState {
        DownloadState {
            monitored_album: true,
            ..state(Some(21), Some(0))
        }
    }

    fn queue_item(
        id: u32,
        status: &str,
        tracked: Option<&str>,
        size: f64,
        sizeleft: f64,
    ) -> lidarr::QueueItem {
        lidarr::QueueItem {
            id,
            album_id: Some(21),
            size,
            sizeleft,
            status: Some(status.to_string()),
            tracked_download_state: tracked.map(str::to_string),
        }
    }

    fn fixture_files() -> Vec<lidarr::TrackFile> {
        fixtures::json("track_files.json")
    }

    #[test]
    fn selects_every_file_when_none_are_named() {
        let candidate = candidate();

        assert_eq!(select(&candidate.files, &[]).unwrap().len(), 2);
        assert_eq!(select(&candidate.files, &[1]).unwrap()[0].title, "Kid A");
    }

    #[test]
    fn rejects_an_index_the_candidate_does_not_offer() {
        assert!(select(&candidate().files, &[3]).is_err());
    }

    #[test]
    fn adds_a_missing_artist_then_monitors_and_searches() {
        let fake = Fake::new();

        let state = prepare(&fake, &FixedClock(Some(100)), &config(), &candidate()).unwrap();

        assert_eq!(state.artist_id, 9);
        assert_eq!(state.album_id, Some(21));
        assert_eq!(state.started_at, Some(100));
        assert!(state.monitored_album);
        assert_eq!(
            fake.calls(),
            vec![
                "artists_by_mbid",
                "add_artist",
                "albums_by_foreign_id",
                "monitor_album",
                "search_album",
            ]
        );

        let added = fake.added.borrow().clone().unwrap();
        assert_eq!(added.artist_name, "Radiohead");
        assert_eq!(added.foreign_artist_id, ARTIST);
        assert_eq!(added.quality_profile_id, 4);
        assert_eq!(added.metadata_profile_id, 2);
        assert_eq!(added.root_folder_path, "/music");
        assert!(added.monitored);
        assert_eq!(added.add_options.monitor, "none");
        assert_eq!(added.add_options.albums_to_monitor, vec![ALBUM]);
        assert!(!added.add_options.search_for_missing_albums);
    }

    #[test]
    fn reuses_an_artist_lidarr_already_holds() {
        let mut fake = Fake::new();
        fake.artists = vec![lidarr::Artist {
            id: 3,
            artist_name: Some("Radiohead".to_string()),
            foreign_artist_id: Some(ARTIST.to_string()),
            root_folder_path: Some("/music".to_string()),
        }];

        let state = prepare(&fake, &FixedClock(Some(100)), &config(), &candidate()).unwrap();

        assert_eq!(state.artist_id, 3);
        assert!(!fake.calls().contains(&"add_artist".to_string()));
        assert_eq!(
            fake.calls(),
            vec![
                "artists_by_mbid",
                "albums_by_foreign_id",
                "monitor_album",
                "search_album",
            ]
        );
    }

    #[test]
    fn waits_for_lidarr_to_list_a_new_albums_artist() {
        let mut fake = Fake::new();
        fake.albums.clear();

        let state = prepare(&fake, &FixedClock(Some(100)), &config(), &candidate()).unwrap();

        assert_eq!(state.album_id, None);
        assert_eq!(
            fake.calls(),
            vec!["artists_by_mbid", "add_artist", "albums_by_foreign_id"]
        );
    }

    #[test]
    fn completes_only_when_every_wanted_file_is_present() {
        let mut fake = Fake::new();
        fake.files = fixture_files();
        let files = Files::with(&[
            "Radiohead/Kid A/01 - Everything In Its Right Place.flac",
            "Radiohead/Kid A/02 - Kid A.flac",
        ]);
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &token(),
            &mut state,
        )
        .unwrap();

        let Advance::Complete(outputs) = outcome else {
            panic!("expected complete, got {outcome:?}");
        };
        assert_eq!(outputs.len(), 2);
        assert_eq!(
            outputs[0].path,
            "Radiohead/Kid A/01 - Everything In Its Right Place.flac"
        );
        assert_eq!(outputs[0].track_index, 0);
        assert_eq!(outputs[1].track_index, 1);
    }

    #[test]
    fn completes_when_only_the_listed_files_are_present() {
        let mut fake = Fake::new();
        fake.files = fixture_files();
        let complete = token();
        let partial = DownloadToken {
            files: complete.files[..1].to_vec(),
            ..complete
        };
        let files = Files::with(&["Radiohead/Kid A/01 - Everything In Its Right Place.flac"]);
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &partial,
            &mut state,
        )
        .unwrap();

        let Advance::Complete(outputs) = outcome else {
            panic!("expected complete, got {outcome:?}");
        };
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].track_index, 0);
    }

    #[test]
    fn returns_only_the_wanted_tracks_lidarr_holds() {
        let mut fake = Fake::new();
        fake.tracks.truncate(1);
        fake.files = fixture_files();
        let files = Files::with(&["Radiohead/Kid A/01 - Everything In Its Right Place.flac"]);
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &token(),
            &mut state,
        )
        .unwrap();

        let Advance::Complete(outputs) = outcome else {
            panic!("expected complete, got {outcome:?}");
        };
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].track_index, 0);
        assert_eq!(
            outputs[0].path,
            "Radiohead/Kid A/01 - Everything In Its Right Place.flac"
        );
    }

    #[test]
    fn finishes_when_lidarr_reports_the_album_imported() {
        let mut fake = Fake::new();
        fake.files = fixture_files();

        for track in &mut fake.tracks {
            track.has_file = true;
        }

        let files = Files::with(&["Radiohead/Kid A/01 - Everything In Its Right Place.flac"]);
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &token(),
            &mut state,
        )
        .unwrap();

        let Advance::Complete(outputs) = outcome else {
            panic!("expected complete, got {outcome:?}");
        };
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].track_index, 0);
    }

    #[test]
    fn waits_while_the_album_is_still_queued() {
        let mut fake = Fake::new();

        for track in &mut fake.tracks {
            track.has_file = true;
        }

        let mut item: lidarr::QueueItem = fixtures::json::<lidarr::QueuePage>("queue.json")
            .records
            .remove(0);
        item.status = Some("queued".to_string());
        item.tracked_download_state = None;
        fake.queue = vec![item];
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &Files::with(&[]),
            &token(),
            &mut state,
        )
        .unwrap();

        assert_eq!(outcome, Advance::Queued(Some(1)));
    }

    #[test]
    fn keeps_waiting_when_one_wanted_file_is_missing() {
        let mut fake = Fake::new();
        fake.files = fixture_files();
        let files = Files::with(&["Radiohead/Kid A/01 - Everything In Its Right Place.flac"]);
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &token(),
            &mut state,
        )
        .unwrap();

        assert_eq!(outcome, Advance::Queued(None));
    }

    #[test]
    fn ignores_a_file_outside_the_root_folder() {
        let mut fake = Fake::new();
        fake.files = fixture_files();
        let files = Files::with(&["other/01 - Everything In Its Right Place.flac"]);
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &token(),
            &mut state,
        )
        .unwrap();

        assert_eq!(outcome, Advance::Queued(None));
        assert_eq!(
            relative_path("/music", "/elsewhere/Artist/Album/01.flac"),
            None
        );
    }

    #[test]
    fn fails_on_a_failed_download_history_event() {
        let mut fake = Fake::new();
        fake.failed = true;
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &Files::with(&[]),
            &token(),
            &mut state,
        )
        .unwrap();

        let Advance::Failed(message) = outcome else {
            panic!("expected failed, got {outcome:?}");
        };
        assert!(message.contains("failed download"), "{message}");
    }

    #[test]
    fn fails_when_no_download_appeared_within_the_wait() {
        let fake = Fake::new();
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(1801)),
            &config(),
            &Files::with(&[]),
            &token(),
            &mut state,
        )
        .unwrap();

        let Advance::Failed(message) = outcome else {
            panic!("expected failed, got {outcome:?}");
        };
        assert!(message.contains("did not start a download"), "{message}");
        assert!(message.contains("30 minutes"), "{message}");
    }

    #[test]
    fn keeps_waiting_before_the_wait_passes() {
        let fake = Fake::new();
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(1799)),
            &config(),
            &Files::with(&[]),
            &token(),
            &mut state,
        )
        .unwrap();

        assert_eq!(outcome, Advance::Queued(None));
    }

    #[test]
    fn reports_download_progress_from_the_queue() {
        let mut fake = Fake::new();
        fake.queue = fixtures::json::<lidarr::QueuePage>("queue.json").records;
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &Files::with(&[]),
            &token(),
            &mut state,
        )
        .unwrap();

        assert_eq!(
            outcome,
            Advance::Running {
                bytes: 3072,
                total_bytes: Some(4096),
            }
        );
    }

    #[test]
    fn reports_held_queue_states_as_queued() {
        for held in HELD_STATES {
            let mut fake = Fake::new();
            fake.queue = vec![queue_item(1, held, Some("downloading"), 4096.0, 1024.0)];
            let mut state = state(Some(21), Some(0));

            let outcome = advance(
                &fake,
                &FixedClock(Some(0)),
                &config(),
                &Files::with(&[]),
                &token(),
                &mut state,
            )
            .unwrap();

            assert_eq!(outcome, Advance::Queued(Some(1)), "status {held}");
        }
    }

    #[test]
    fn reports_an_importing_item_as_queued() {
        for tracked in IMPORT_STATES {
            let mut fake = Fake::new();
            fake.queue = vec![queue_item(1, "downloading", Some(tracked), 4096.0, 1024.0)];
            let mut state = state(Some(21), Some(0));

            let outcome = advance(
                &fake,
                &FixedClock(Some(0)),
                &config(),
                &Files::with(&[]),
                &token(),
                &mut state,
            )
            .unwrap();

            assert_eq!(outcome, Advance::Queued(Some(1)), "state {tracked}");
        }
    }

    #[test]
    fn reports_a_finished_transfer_waiting_for_import_as_queued() {
        let mut fake = Fake::new();
        fake.queue = vec![queue_item(
            1,
            "downloading",
            Some("downloading"),
            4096.0,
            0.0,
        )];
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &Files::with(&[]),
            &token(),
            &mut state,
        )
        .unwrap();

        assert_eq!(outcome, Advance::Queued(Some(1)));
    }

    #[test]
    fn reports_a_transfer_without_bytes_yet_as_queued() {
        let mut fake = Fake::new();
        fake.queue = vec![queue_item(
            1,
            "downloading",
            Some("downloading"),
            4096.0,
            4096.0,
        )];
        let mut state = state(Some(21), Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &Files::with(&[]),
            &token(),
            &mut state,
        )
        .unwrap();

        assert_eq!(outcome, Advance::Queued(Some(1)));
    }

    #[test]
    fn reports_running_only_while_the_byte_count_advances() {
        let mut fake = Fake::new();
        fake.queue = fixtures::json::<lidarr::QueuePage>("queue.json").records;
        let mut state = state(Some(21), Some(0));
        let files = Files::with(&[]);

        let first = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &token(),
            &mut state,
        )
        .unwrap();
        assert_eq!(
            first,
            Advance::Running {
                bytes: 3072,
                total_bytes: Some(4096),
            }
        );

        let second = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &token(),
            &mut state,
        )
        .unwrap();
        assert_eq!(second, Advance::Queued(Some(1)));

        fake.queue[0].sizeleft = 512.0;
        let third = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &files,
            &token(),
            &mut state,
        )
        .unwrap();
        assert_eq!(
            third,
            Advance::Running {
                bytes: 3584,
                total_bytes: Some(4096),
            }
        );
    }

    #[test]
    fn monitors_and_searches_once_lidarr_lists_the_album() {
        let fake = Fake::new();
        let mut state = state(None, Some(0));

        let outcome = advance(
            &fake,
            &FixedClock(Some(0)),
            &config(),
            &Files::with(&[]),
            &token(),
            &mut state,
        )
        .unwrap();

        assert_eq!(state.album_id, Some(21));
        assert!(state.monitored_album);
        assert_eq!(
            fake.calls(),
            vec![
                "albums_by_foreign_id",
                "monitor_album",
                "search_album",
                "tracks",
                "track_files",
                "queue",
                "failed_download"
            ]
        );
        assert_eq!(outcome, Advance::Queued(None));
    }

    #[test]
    fn cancels_the_queue_items_then_unmonitors_the_album() {
        let mut fake = Fake::new();
        fake.queue = vec![
            queue_item(1, "downloading", Some("downloading"), 4096.0, 1024.0),
            queue_item(2, "queued", Some("queued"), 8192.0, 8192.0),
        ];

        cancel(&fake, Some(&monitored_state()), &token()).unwrap();

        assert_eq!(fake.removed(), vec![(1, true), (2, true)]);
        assert_eq!(fake.unmonitored(), vec![21]);
        assert_eq!(
            fake.calls(),
            vec![
                "queue",
                "remove_queue_item",
                "remove_queue_item",
                "unmonitor_album",
            ]
        );
    }

    #[test]
    fn cancel_is_a_no_op_without_state() {
        let fake = Fake::new();

        cancel(&fake, None, &token()).unwrap();

        assert!(fake.calls().is_empty());
        assert!(fake.removed().is_empty());
        assert!(fake.unmonitored().is_empty());
    }

    #[test]
    fn cancel_keeps_an_album_the_plugin_did_not_monitor() {
        let mut fake = Fake::new();
        fake.queue = vec![queue_item(
            1,
            "downloading",
            Some("downloading"),
            4096.0,
            1024.0,
        )];

        cancel(&fake, Some(&state(Some(21), Some(0))), &token()).unwrap();

        assert_eq!(fake.removed(), vec![(1, true)]);
        assert!(fake.unmonitored().is_empty());
    }

    #[test]
    fn cancel_unmonitors_when_a_queue_item_is_already_gone() {
        let mut fake = Fake::new();
        fake.fail_queue_removal = true;
        fake.queue = vec![queue_item(
            1,
            "downloading",
            Some("downloading"),
            4096.0,
            1024.0,
        )];

        let error = cancel(&fake, Some(&monitored_state()), &token()).unwrap_err();

        assert!(matches!(error, ProviderError::NotFound));
        assert_eq!(fake.unmonitored(), vec![21]);
    }

    #[test]
    fn cancel_reports_a_failed_unmonitor() {
        let mut fake = Fake::new();
        fake.fail_unmonitor = true;

        let error = cancel(&fake, Some(&monitored_state()), &token()).unwrap_err();

        assert!(matches!(error, ProviderError::Unavailable(_)));
        assert_eq!(fake.unmonitored(), vec![21]);
    }

    #[test]
    fn cancel_finds_the_album_when_the_state_arrived_before_lidarr_listed_it() {
        let fake = Fake::new();
        let state = DownloadState {
            album_id: None,
            ..monitored_state()
        };

        cancel(&fake, Some(&state), &token()).unwrap();

        assert_eq!(fake.unmonitored(), vec![21]);
        assert!(fake.calls().contains(&"albums_by_foreign_id".to_string()));
    }

    #[test]
    fn matches_track_files_by_tag_then_by_title_in_the_path() {
        let wanted = candidate().files;
        let files = fixture_files();

        assert_eq!(match_files(&wanted, &files), vec![Some(0), Some(1)]);

        let renamed = vec![lidarr::TrackFile {
            id: 2,
            path: Some("/music/Radiohead/Kid A/02 - Kid A.flac".to_string()),
            size: 2048,
            audio_tags: None,
        }];
        assert_eq!(match_files(&wanted, &renamed), vec![None, Some(0)]);
    }

    #[test]
    fn rejects_relative_paths_that_leave_the_root() {
        assert_eq!(relative_path("/music", "/music/../secret.flac"), None);
        assert_eq!(relative_path("/music", "/music/"), None);
        assert_eq!(relative_path("", "/music/song.flac"), None);
        assert_eq!(
            relative_path("/music", "/music/Artist/song.flac").as_deref(),
            Some("Artist/song.flac")
        );
    }
}
