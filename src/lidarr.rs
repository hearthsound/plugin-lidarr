//! Serde types for the parts of the Lidarr API this plugin uses.
//!
//! Lidarr answers with large resources; these structs declare only the members
//! the plugin reads, and every optional member tolerates absence because Lidarr
//! omits fields that are null. The API shapes follow Lidarr's published v1
//! OpenAPI document.
//!
//! Everything here is pure: it never touches the network, so it is unit-tested
//! on the host target.

use crate::clock;

/// An artist as Lidarr reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    /// Lidarr's numeric artist id.
    #[serde(default)]
    pub id: u32,
    /// The artist's name.
    #[serde(default)]
    pub artist_name: Option<String>,
    /// The artist's MusicBrainz id, which is Lidarr's foreign artist id.
    #[serde(default)]
    pub foreign_artist_id: Option<String>,
    /// Lidarr's root folder path for the artist.
    #[serde(default)]
    pub root_folder_path: Option<String>,
}

/// An album as Lidarr reports it, for a lookup result and for a library row.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    /// Lidarr's numeric album id, and `0` before the album is in the library.
    #[serde(default)]
    pub id: u32,
    /// The album title.
    #[serde(default)]
    pub title: Option<String>,
    /// The album's release-group MusicBrainz id, which is Lidarr's foreign
    /// album id.
    #[serde(default)]
    pub foreign_album_id: Option<String>,
    /// Lidarr's numeric id of the album's artist.
    #[serde(default)]
    pub artist_id: u32,
    /// The artist, present on lookup results.
    #[serde(default)]
    pub artist: Option<Artist>,
    /// The album's releases, present on lookup results.
    #[serde(default)]
    pub releases: Vec<Release>,
}

/// One release of an album as Lidarr reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// The release title.
    #[serde(default)]
    pub title: Option<String>,
    /// How many tracks the release holds.
    #[serde(default)]
    pub track_count: Option<u32>,
    /// Whether Lidarr monitors this release of the album.
    #[serde(default)]
    pub monitored: bool,
    /// The release's tracklist. Lidarr omits it on most lookup results.
    #[serde(default)]
    pub tracks: Vec<ReleaseTrack>,
}

/// One track of a release as Lidarr reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseTrack {
    /// The track title.
    #[serde(default)]
    pub title: Option<String>,
}

/// A quality profile as Lidarr reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityProfile {
    /// The profile's id.
    #[serde(default)]
    pub id: u32,
    /// The id of the quality the profile upgrades to and stops at.
    #[serde(default)]
    pub cutoff: i64,
    /// The profile's allowed qualities, possibly grouped.
    #[serde(default)]
    pub items: Vec<QualityProfileItem>,
}

/// One entry of a quality profile, either a quality or a group of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityProfileItem {
    /// The entry's id, which a profile cutoff can name.
    #[serde(default)]
    pub id: u32,
    /// The quality, absent on a group.
    #[serde(default)]
    pub quality: Option<Quality>,
    /// The group's entries, absent on a quality.
    #[serde(default)]
    pub items: Vec<QualityProfileItem>,
}

/// A quality as Lidarr names it, for example `FLAC` or `MP3-320`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    /// The quality's id.
    #[serde(default)]
    pub id: i64,
    /// The quality's name.
    #[serde(default)]
    pub name: Option<String>,
}

/// One item of Lidarr's download queue.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    /// The album the item belongs to.
    #[serde(default)]
    pub album_id: Option<u32>,
    /// The remaining size of the download, in bytes.
    #[serde(default)]
    pub sizeleft: f64,
    /// The full size of the download, in bytes.
    #[serde(default)]
    pub size: f64,
    /// The download client's status, for example `downloading`.
    #[serde(default)]
    pub status: Option<String>,
    /// Lidarr's tracked state of the download.
    #[serde(default)]
    pub tracked_download_state: Option<String>,
}

/// A page of Lidarr's queue.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePage {
    /// The page's records.
    #[serde(default)]
    pub records: Vec<QueueItem>,
}

/// One track of an album in Lidarr's library.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// Lidarr's numeric track id.
    #[serde(default)]
    pub id: u32,
    /// The track title.
    #[serde(default)]
    pub title: Option<String>,
    /// Whether Lidarr holds a file for the track.
    #[serde(default)]
    pub has_file: bool,
    /// The track file Lidarr imported, when it holds one.
    #[serde(default)]
    pub track_file_id: Option<u32>,
}

/// One track file Lidarr has imported for an album.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackFile {
    /// Lidarr's numeric file id, which a track names.
    #[serde(default)]
    pub id: u32,
    /// The file's path on Lidarr's filesystem.
    #[serde(default)]
    pub path: Option<String>,
    /// The file's size in bytes.
    #[serde(default)]
    pub size: u64,
    /// Lidarr's parsed tags of the file.
    #[serde(default)]
    pub audio_tags: Option<TrackTags>,
}

/// The tags Lidarr parsed from a track file.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackTags {
    /// The track title.
    #[serde(default)]
    pub title: Option<String>,
    /// The track numbers the tags carry.
    #[serde(default)]
    pub track_numbers: Vec<u32>,
}

/// One history record.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    /// The album the event belongs to.
    #[serde(default)]
    pub album_id: u32,
    /// The event type, for example `downloadFailed`.
    #[serde(default)]
    pub event_type: Option<String>,
    /// When the event happened.
    #[serde(default)]
    pub date: Option<String>,
}

/// A page of Lidarr's history.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    /// The page's records.
    #[serde(default)]
    pub records: Vec<HistoryRecord>,
}

/// Lidarr's system status.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    /// The Lidarr version running on the instance.
    #[serde(default)]
    pub version: Option<String>,
}

/// The body `POST /api/v1/artist` accepts for a new artist.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddArtist {
    /// The artist's name.
    pub artist_name: String,
    /// The artist's MusicBrainz id.
    pub foreign_artist_id: String,
    /// The quality profile to store the artist with.
    pub quality_profile_id: u32,
    /// The metadata profile to store the artist with.
    pub metadata_profile_id: u32,
    /// Lidarr's root folder for the artist.
    pub root_folder_path: String,
    /// Whether the artist itself is monitored.
    pub monitored: bool,
    /// How Lidarr monitors the artist's albums during the add.
    pub add_options: AddArtistOptions,
}

/// The add options of [`AddArtist`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddArtistOptions {
    /// The album monitor mode; `none` adds the artist without monitoring an
    /// album.
    pub monitor: String,
    /// Release-group ids to monitor during the add anyway.
    pub albums_to_monitor: Vec<String>,
    /// Whether Lidarr searches for missing albums after the add; the plugin
    /// starts its own search for the one album it wants.
    pub search_for_missing_albums: bool,
}

/// The body `PUT /api/v1/album/monitor` accepts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumsMonitored {
    /// Ids of the albums to monitor.
    pub album_ids: Vec<u32>,
    /// Whether the albums are monitored.
    pub monitored: bool,
}

/// The body `POST /api/v1/command` accepts for an album search.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumSearchCommand {
    /// The command name, always `AlbumSearch`.
    pub name: String,
    /// Ids of the albums to search for.
    pub album_ids: Vec<u32>,
}

/// Returns whether history holds a failed download for an album since a time.
///
/// A failure older than the acquisition that asks is ignored: Lidarr keeps its
/// history, and a previous failure must not fail an acquisition that has not
/// failed. A record whose date cannot be parsed counts as recent, because
/// silently ignoring an unreadable failure would hide it.
///
/// # Parameters
///
/// - `records`: history records for the album.
/// - `since`: when the current acquisition started, in seconds since the Unix
///   epoch; `None` means every record counts.
///
/// # Returns
///
/// `true` when at least one record is a `downloadFailed` event at or after
/// `since`.
///
/// # Example
///
/// ```text
/// has_failed_download([record("downloadFailed", "2024-05-01T00:00:00Z")], Some(0)) -> true
/// ```
pub fn has_failed_download(records: &[HistoryRecord], since: Option<i64>) -> bool {
    records.iter().any(|record| {
        if !record
            .event_type
            .as_deref()
            .is_some_and(|event| event.eq_ignore_ascii_case("downloadFailed"))
        {
            return false;
        }

        match (since, record.date.as_deref().and_then(clock::parse_iso_utc)) {
            (Some(since), Some(date)) => date >= since,
            (Some(_), None) => true,
            (None, _) => true,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    #[test]
    fn reads_a_lookup_result() {
        let albums: Vec<Album> = fixtures::json("lookup.json");

        assert_eq!(albums.len(), 1);
        let album = &albums[0];
        assert_eq!(album.title.as_deref(), Some("Kid A"));
        assert_eq!(
            album.foreign_album_id.as_deref(),
            Some("f0f0f0f0-0000-0000-0000-000000000001")
        );
        let artist = album.artist.as_ref().unwrap();
        assert_eq!(artist.artist_name.as_deref(), Some("Radiohead"));
        assert_eq!(
            artist.foreign_artist_id.as_deref(),
            Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
        );
    }

    #[test]
    fn reads_a_lookup_result_with_its_releases() {
        let albums: Vec<Album> = fixtures::json("lookup_tracks.json");
        let album = &albums[0];

        assert_eq!(album.releases.len(), 2);
        assert_eq!(album.releases[0].track_count, Some(13));
        assert!(album.releases[0].monitored);
        assert_eq!(album.releases[0].tracks.len(), 13);
        assert_eq!(
            album.releases[0].tracks[0].title.as_deref(),
            Some("Cry Baby")
        );
    }

    #[test]
    fn reads_a_quality_profile() {
        let profile: QualityProfile = fixtures::json("quality_profile.json");

        assert_eq!(profile.cutoff, 1);
        assert_eq!(profile.items.len(), 3);
    }

    #[test]
    fn reads_the_tracks_of_an_album() {
        let tracks: Vec<Track> = fixtures::json("tracks.json");

        assert_eq!(tracks.len(), 2);
        assert_eq!(
            tracks[0].title.as_deref(),
            Some("Everything In Its Right Place")
        );
        assert!(!tracks[0].has_file);
        assert_eq!(tracks[0].track_file_id, None);
    }

    #[test]
    fn reads_queue_track_files_and_history_pages() {
        let queue: QueuePage = fixtures::json("queue.json");
        assert_eq!(queue.records.len(), 1);
        assert_eq!(queue.records[0].album_id, Some(21));
        assert_eq!(queue.records[0].status.as_deref(), Some("downloading"));

        let files: Vec<TrackFile> = fixtures::json("track_files.json");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].size, 1024);

        let history: HistoryPage = fixtures::json("history_failed.json");
        assert!(has_failed_download(&history.records, Some(0)));
    }

    #[test]
    fn ignores_a_failure_before_the_acquisition() {
        let history: HistoryPage = fixtures::json("history_failed.json");
        let failed_at = clock::parse_iso_utc("2024-05-01T00:00:00Z").unwrap();

        assert!(!has_failed_download(&history.records, Some(failed_at + 1)));
        assert!(has_failed_download(&history.records, Some(failed_at)));
        assert!(has_failed_download(&history.records, Some(failed_at - 1)));
    }

    #[test]
    fn ignores_records_that_are_not_failures() {
        let records = vec![HistoryRecord {
            album_id: 21,
            event_type: Some("trackFileImported".to_string()),
            date: Some("2024-05-01T00:00:00Z".to_string()),
        }];

        assert!(!has_failed_download(&records, None));
    }

    #[test]
    fn treats_an_unreadable_failure_date_as_recent() {
        let records = vec![HistoryRecord {
            album_id: 21,
            event_type: Some("downloadFailed".to_string()),
            date: None,
        }];

        assert!(has_failed_download(&records, Some(0)));
    }
}
