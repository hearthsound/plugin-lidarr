//! Album lookup terms and the mapping from Lidarr lookup results to
//! candidates.
//!
//! Lidarr resolves an album lookup either by its release-group MusicBrainz id
//! (`lidarr:<id>`) or by text; the id is exact, so it is tried first when the
//! query carries one. A lookup is synchronous, so the search completes in one
//! poll and the candidates travel in the search's store entry.
//!
//! Everything here is pure: it never touches the network, so it is unit-tested
//! on the host target.

use hearthsound_plugin::acquisition_provider::exports::hearthsound::plugin::acquisition::{
    Candidate, CandidateFile, Query, SourceInfo,
};
use hearthsound_plugin::types::ProviderError;

use crate::lidarr;
use crate::quality;

/// The candidate source key every candidate of this plugin carries.
pub const SOURCE_KEY: &str = "lidarr";

/// Most lookup results that become candidates.
pub const MAX_CANDIDATES: usize = 3;

/// The token `start-search` returns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchToken {
    /// Key of the host-kv entry that holds the search's candidates.
    pub key: String,
}

/// The candidates of one search, as they are kept between calls.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSearch {
    /// The candidates, in lookup order.
    pub candidates: Vec<CandidateToken>,
}

/// The token a candidate carries, and everything `start-download` needs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateToken {
    /// The album's release-group MusicBrainz id.
    pub album_foreign_id: String,
    /// The album title Lidarr reported.
    pub album_title: String,
    /// The artist's MusicBrainz id.
    pub artist_foreign_id: String,
    /// The artist name Lidarr reported.
    pub artist_name: String,
    /// The audio codec the quality profile's cutoff implies.
    pub format: String,
    /// The bitrate the cutoff names, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    /// The wanted tracks, in the query's order.
    pub files: Vec<WantedFile>,
}

/// One wanted track, as the candidate and the download remember it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WantedFile {
    /// The track's position in the query's track list.
    pub index: u32,
    /// The track title.
    pub title: String,
    /// The artist names in credit order.
    pub artists: Vec<String>,
    /// The track number on its medium.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u16>,
    /// The track's duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
}

/// Builds the lookup terms of one query, in the order they are tried.
///
/// # Parameters
///
/// - `query`: the album query the server sent.
///
/// # Returns
///
/// `lidarr:<release-group id>` followed by `<album artist> <album>` when any
/// wanted track carries an `mb-release` id, and only the text otherwise. The
/// plugin stops at the first term that produces a candidate, so the text is the
/// fallback for an instance whose own lookup does not resolve the id.
///
/// # Errors
///
/// Returns [`ProviderError::Internal`] when the query names no album and
/// carries no release-group id, because there is nothing to look up.
///
/// # Example
///
/// ```text
/// terms(album "Kid A", artist "Radiohead") -> ["Radiohead Kid A"]
/// ```
pub fn terms(query: &Query) -> Result<Vec<String>, ProviderError> {
    let text = text_term(query)?;

    let release_group = query
        .tracks
        .iter()
        .flat_map(|track| &track.ids)
        .find(|id| id.namespace == "mb-release" && !id.value.trim().is_empty());

    match release_group {
        Some(id) => {
            let exact = format!("{SOURCE_KEY}:{}", id.value.trim());

            Ok(if exact == text {
                vec![exact]
            } else {
                vec![exact, text]
            })
        }
        None => Ok(vec![text]),
    }
}

/// Builds the text lookup term of one query.
///
/// # Parameters
///
/// - `query`: the album query.
///
/// # Returns
///
/// `<album artist> <album>`, or the album title when the query names no album
/// artist.
///
/// # Errors
///
/// Returns [`ProviderError::Internal`] when the query names no album.
fn text_term(query: &Query) -> Result<String, ProviderError> {
    let album = query.album.as_deref().map(collapse).unwrap_or_default();
    if album.is_empty() {
        return Err(ProviderError::Internal(
            "the query names no album".to_string(),
        ));
    }

    let artist = query
        .album_artist
        .as_deref()
        .map(collapse)
        .unwrap_or_default();

    Ok(if artist.is_empty() {
        album
    } else {
        format!("{artist} {album}")
    })
}

/// Looks up the query's terms in order and stops at the first candidate.
///
/// # Parameters
///
/// - `query`: the album query, which supplies the wanted tracks.
/// - `terms`: the lookup terms, in the order they are tried.
/// - `mapping`: the format the quality profile's cutoff implies.
/// - `lookup`: the album lookup, one call per term.
///
/// # Returns
///
/// The candidates of the first term that produced any, and an empty list when
/// every term produced none.
///
/// # Errors
///
/// Returns the error the lookup reports.
pub(crate) fn find<F>(
    query: &Query,
    terms: &[String],
    mapping: &quality::Mapping,
    mut lookup: F,
) -> Result<Vec<CandidateToken>, ProviderError>
where
    F: FnMut(&str) -> Result<Vec<lidarr::Album>, ProviderError>,
{
    for term in terms {
        let albums = lookup(term)?;
        let found = candidates(query, &albums, mapping);

        if !found.is_empty() {
            return Ok(found);
        }
    }

    Ok(Vec::new())
}

/// Converts lookup results into candidates.
///
/// # Parameters
///
/// - `query`: the album query, which supplies the wanted tracks.
/// - `albums`: the album lookup results Lidarr returned.
/// - `mapping`: the format the quality profile's cutoff implies.
///
/// # Returns
///
/// At most [`MAX_CANDIDATES`] candidates, in lookup order. A result Lidarr did
/// not identify by release-group and artist is skipped, because the plugin
/// cannot add or monitor it later.
pub fn candidates(
    query: &Query,
    albums: &[lidarr::Album],
    mapping: &quality::Mapping,
) -> Vec<CandidateToken> {
    albums
        .iter()
        .filter_map(|album| candidate(query, album, mapping))
        .take(MAX_CANDIDATES)
        .collect()
}

/// Converts one lookup result into a candidate.
///
/// # Parameters
///
/// - `query`: the album query.
/// - `album`: the lookup result.
/// - `mapping`: the cutoff mapping.
///
/// # Returns
///
/// The candidate, and `None` when the result lacks an album id, an artist id or
/// a title.
fn candidate(
    query: &Query,
    album: &lidarr::Album,
    mapping: &quality::Mapping,
) -> Option<CandidateToken> {
    let album_foreign_id = album.foreign_album_id.as_deref()?.trim().to_string();
    let album_title = album.title.as_deref()?.trim().to_string();
    let artist = album.artist.as_ref()?;
    let artist_foreign_id = artist.foreign_artist_id.as_deref()?.trim().to_string();
    let artist_name = artist
        .artist_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Unknown Artist")
        .to_string();

    if album_foreign_id.is_empty() || album_title.is_empty() || artist_foreign_id.is_empty() {
        return None;
    }

    Some(CandidateToken {
        album_foreign_id,
        album_title,
        artist_foreign_id,
        artist_name,
        format: mapping.format.clone(),
        bitrate_kbps: mapping.bitrate_kbps,
        files: query
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| WantedFile {
                index: index as u32,
                title: track.title.clone(),
                artists: track.artists.clone(),
                track_number: track.track_number,
                duration_ms: track.duration_ms,
            })
            .collect(),
    })
}

/// Renders a stored candidate as the interface's record.
///
/// # Parameters
///
/// - `token`: the stored candidate.
/// - `encoded`: the candidate token the server hands back to
///   `start-download`.
///
/// # Returns
///
/// The candidate record, with one file per wanted track.
pub fn view(token: &CandidateToken, encoded: String) -> Candidate {
    Candidate {
        token: encoded,
        title: token.album_title.clone(),
        path: format!("{} - {}", token.artist_name, token.album_title),
        files: token
            .files
            .iter()
            .map(|file| CandidateFile {
                name: file_name(file, &token.format),
                size: None,
                format: Some(token.format.clone()),
                bitrate_kbps: token.bitrate_kbps,
                sample_rate: None,
                bit_depth: None,
                duration_ms: file.duration_ms,
                track_index: Some(file.index),
            })
            .collect(),
        source: SourceInfo {
            key: SOURCE_KEY.to_string(),
            speed_bps: None,
            queue_length: None,
            free_slot: None,
        },
    }
}

/// Returns the display name of one candidate file.
///
/// Lidarr chooses the real file name itself; this name tells an administrator
/// which track the file is for.
///
/// # Parameters
///
/// - `file`: the wanted track.
/// - `format`: the codec the candidate expects.
///
/// # Returns
///
/// `<NN> - <title>.<ext>`, using the track number when the query has one and
/// the track's position otherwise.
///
/// # Example
///
/// ```text
/// file_name(track 1 "Kid A", "flac") -> "01 - Kid A.flac"
/// ```
pub fn file_name(file: &WantedFile, format: &str) -> String {
    let number = file
        .track_number
        .map_or(file.index.saturating_add(1), u32::from);
    let title = sanitize(&file.title);

    format!("{number:02} - {title}.{format}")
}

/// Makes a title safe for a path segment.
///
/// # Parameters
///
/// - `title`: the track title.
///
/// # Returns
///
/// The title with path separators and the characters Windows forbids replaced
/// by `-`, whitespace collapsed, and `Track <n>` when nothing usable remains.
fn sanitize(title: &str) -> String {
    let collapsed = collapse(title);
    let sanitized: String = collapsed
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            character if character.is_control() => '-',
            character => character,
        })
        .collect();
    let sanitized = sanitized.trim().trim_matches('-').trim();

    if sanitized.is_empty() {
        "Track".to_string()
    } else {
        sanitized.to_string()
    }
}

/// Collapses whitespace around and inside a value.
///
/// # Parameters
///
/// - `value`: the value to collapse.
///
/// # Returns
///
/// The trimmed value with every whitespace run replaced by one space.
pub fn collapse(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use hearthsound_plugin::acquisition_provider::exports::hearthsound::plugin::acquisition::{
        Granularity, WantedTrack,
    };
    use hearthsound_plugin::types::IdPair;
    use std::cell::RefCell;

    fn track(title: &str, number: u16) -> WantedTrack {
        WantedTrack {
            title: title.to_string(),
            artists: vec!["Radiohead".to_string()],
            track_number: Some(number),
            disc_number: Some(1),
            duration_ms: Some(240_000),
            isrc: None,
            ids: Vec::new(),
        }
    }

    fn query(tracks: Vec<WantedTrack>) -> Query {
        Query {
            granularity: Granularity::Album,
            album: Some("Kid A".to_string()),
            album_artist: Some("Radiohead".to_string()),
            year: Some(2000),
            tracks,
            preferred_formats: vec!["flac".to_string()],
        }
    }

    #[test]
    fn tries_a_release_group_id_before_the_text() {
        let mut first = track("Everything In Its Right Place", 1);
        first.ids.push(IdPair {
            namespace: "mb-release".to_string(),
            value: "f0f0f0f0-0000-0000-0000-000000000001".to_string(),
        });

        assert_eq!(
            terms(&query(vec![first])).unwrap(),
            vec![
                "lidarr:f0f0f0f0-0000-0000-0000-000000000001".to_string(),
                "Radiohead Kid A".to_string(),
            ]
        );
    }

    #[test]
    fn falls_back_to_the_artist_and_album_text() {
        assert_eq!(
            terms(&query(vec![track("Kid A", 1)])).unwrap(),
            vec!["Radiohead Kid A".to_string()]
        );
    }

    #[test]
    fn refuses_a_query_without_an_album() {
        let mut query = query(Vec::new());
        query.album = None;
        query.tracks.push(track("Kid A", 1));

        assert!(terms(&query).is_err());
    }

    #[test]
    fn converts_lookup_results_into_candidates() {
        let albums: Vec<lidarr::Album> = fixtures::json("lookup.json");
        let found = candidates(
            &query(vec![
                track("Everything In Its Right Place", 1),
                track("Kid A", 2),
            ]),
            &albums,
            &quality::Mapping {
                format: "flac".to_string(),
                bitrate_kbps: None,
            },
        );

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].album_foreign_id,
            "f0f0f0f0-0000-0000-0000-000000000001"
        );
        assert_eq!(found[0].artist_name, "Radiohead");
        assert_eq!(found[0].files.len(), 2);
        assert_eq!(found[0].files[1].index, 1);
        assert_eq!(found[0].files[1].title, "Kid A");
    }

    #[test]
    fn falls_back_to_the_text_lookup_when_the_id_finds_nothing() {
        let albums: Vec<lidarr::Album> = fixtures::json("lookup.json");
        let calls = RefCell::new(Vec::new());
        let mut answers = vec![Vec::new(), albums].into_iter();
        let found = find(
            &query(vec![track("Kid A", 1)]),
            &[
                "lidarr:f0f0f0f0-0000-0000-0000-000000000001".to_string(),
                "Radiohead Kid A".to_string(),
            ],
            &quality::Mapping {
                format: "flac".to_string(),
                bitrate_kbps: None,
            },
            |term| {
                calls.borrow_mut().push(term.to_string());

                Ok(answers.next().unwrap_or_default())
            },
        )
        .unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(
            calls.borrow().as_slice(),
            [
                "lidarr:f0f0f0f0-0000-0000-0000-000000000001",
                "Radiohead Kid A"
            ]
        );
    }

    #[test]
    fn stops_looking_after_the_first_term_that_finds_a_candidate() {
        let albums: Vec<lidarr::Album> = fixtures::json("lookup.json");
        let calls = RefCell::new(Vec::new());
        let found = find(
            &query(vec![track("Kid A", 1)]),
            &["Radiohead Kid A".to_string(), "Kid A".to_string()],
            &quality::Mapping {
                format: "flac".to_string(),
                bitrate_kbps: None,
            },
            |term| {
                calls.borrow_mut().push(term.to_string());

                Ok(albums.clone())
            },
        )
        .unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(calls.borrow().as_slice(), ["Radiohead Kid A"]);
    }

    #[test]
    fn skips_results_that_cannot_be_added() {
        let album = lidarr::Album {
            foreign_album_id: Some("x".to_string()),
            title: Some("Unknown".to_string()),
            ..lidarr::Album::default()
        };

        assert!(
            candidates(
                &query(vec![track("Kid A", 1)]),
                &[album],
                &quality::Mapping {
                    format: "flac".to_string(),
                    bitrate_kbps: None,
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn renders_candidate_files_per_wanted_track() {
        let token = CandidateToken {
            album_foreign_id: "rg".to_string(),
            album_title: "Kid A".to_string(),
            artist_foreign_id: "artist".to_string(),
            artist_name: "Radiohead".to_string(),
            format: "mp3".to_string(),
            bitrate_kbps: Some(320),
            files: vec![
                WantedFile {
                    index: 0,
                    title: "Everything In Its Right Place".to_string(),
                    artists: vec!["Radiohead".to_string()],
                    track_number: Some(1),
                    duration_ms: Some(240_000),
                },
                WantedFile {
                    index: 1,
                    title: "Kid A/Amnesiac".to_string(),
                    artists: vec!["Radiohead".to_string()],
                    track_number: None,
                    duration_ms: Some(200_000),
                },
            ],
        };
        let candidate = view(&token, "token".to_string());

        assert_eq!(candidate.token, "token");
        assert_eq!(
            candidate.files[0].name,
            "01 - Everything In Its Right Place.mp3"
        );
        assert_eq!(candidate.files[0].track_index, Some(0));
        assert_eq!(candidate.files[0].format.as_deref(), Some("mp3"));
        assert_eq!(candidate.files[0].bitrate_kbps, Some(320));
        assert_eq!(candidate.files[1].name, "02 - Kid A-Amnesiac.mp3");
        assert_eq!(candidate.source.key, "lidarr");
    }
}
