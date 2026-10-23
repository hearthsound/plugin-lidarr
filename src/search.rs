//! Album lookup terms and the mapping from Lidarr lookup results to
//! candidates.
//!
//! Lidarr resolves an album lookup either by its release-group MusicBrainz id
//! (`lidarr:<id>`) or by text; the id is exact, so it is tried first when the
//! query carries one. A lookup is synchronous, so the search completes in one
//! poll and the candidates travel in the search's store entry.
//!
//! Lidarr's text search matches on any word, so a lookup for one album can
//! return unrelated albums that share a word. Only results that name the
//! requested album, by normalized artist and title, become candidates, and a
//! candidate's files come from the release tracklist Lidarr sent whenever it
//! sent one.
//!
//! Everything here is pure: it never touches the network, so it is unit-tested
//! on the host target.

use hearthsound_plugin::acquisition_provider::exports::hearthsound::plugin::acquisition::{
    Candidate, CandidateFile, Query, SourceInfo,
};
use hearthsound_plugin::types::ProviderError;
use unicode_normalization::UnicodeNormalization;

use crate::lidarr;
use crate::quality;

/// The candidate source key every candidate of this plugin carries.
pub const SOURCE_KEY: &str = "lidarr";

/// Most lookup results that become candidates.
pub const MAX_CANDIDATES: usize = 3;

/// Words that mark a trailing bracket group as an edition qualifier.
///
/// A qualifier such as `(Deluxe Edition)` or `[Remastered]` names a variant of
/// the same album, so it is ignored when a lookup result is matched against the
/// requested album. A group with any other word, such as `(Live)`, stays part of
/// the title.
const EDITION_WORDS: [&str; 12] = [
    "anniversary",
    "bonus",
    "collector",
    "collectors",
    "deluxe",
    "edition",
    "expanded",
    "limited",
    "reissue",
    "remaster",
    "remastered",
    "special",
];

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
/// wanted track carries an `mb-release-group` id, and only the text otherwise. The
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
        .find(|id| id.namespace == "mb-release-group" && !id.value.trim().is_empty());

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
/// At most [`MAX_CANDIDATES`] candidates, in lookup order. A result becomes a
/// candidate only when its artist equals the query's album artist and its title
/// equals the query's album title, ignoring case, accents, punctuation and a
/// trailing edition qualifier; Lidarr's text search matches on any word, so
/// without that check an unrelated album that shares a word could be acquired
/// instead. A result Lidarr did not identify by release-group and artist is
/// skipped, because the plugin cannot add or monitor it later, and so is a
/// result whose tracklist shares no title with the wanted tracks.
pub fn candidates(
    query: &Query,
    albums: &[lidarr::Album],
    mapping: &quality::Mapping,
) -> Vec<CandidateToken> {
    albums
        .iter()
        .filter_map(|album| candidate(query, album, mapping))
        .filter(|candidate| !candidate.files.is_empty())
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
/// a title, or when it does not name the requested album.
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

    if !names_the_requested_album(query, &album_title, &artist_name) {
        return None;
    }

    Some(CandidateToken {
        album_foreign_id,
        album_title,
        artist_foreign_id,
        artist_name,
        format: mapping.format.clone(),
        bitrate_kbps: mapping.bitrate_kbps,
        files: wanted_files(query, album),
    })
}

/// Reports whether a lookup result names the album the query asks for.
///
/// # Parameters
///
/// - `query`: the album query.
/// - `title`: the lookup result's album title.
/// - `artist`: the lookup result's artist name.
///
/// # Returns
///
/// `true` when the normalized titles are equal - with a trailing edition
/// qualifier removed from both - and, when the query names an album artist,
/// the normalized artist names are equal too.
fn names_the_requested_album(query: &Query, title: &str, artist: &str) -> bool {
    let Some(wanted_album) = query.album.as_deref() else {
        return false;
    };
    let wanted_title = normalize(&without_edition(wanted_album));

    if wanted_title.is_empty() || wanted_title != normalize(&without_edition(title)) {
        return false;
    }

    match query.album_artist.as_deref().map(normalize) {
        Some(wanted_artist) if !wanted_artist.is_empty() => wanted_artist == normalize(artist),
        _ => true,
    }
}

/// Lists the files one candidate offers.
///
/// # Parameters
///
/// - `query`: the album query, whose tracks are the wanted ones.
/// - `album`: the lookup result.
///
/// # Returns
///
/// One file per wanted track that also appears by normalized title in the
/// release tracklist Lidarr sent, in the query's order. A release Lidarr
/// monitors is preferred because that is the release its search downloads, and
/// the release with the most tracks is used when none is monitored. Lidarr
/// usually omits tracklists from lookup results; then a file is listed per
/// wanted track, because the plugin cannot know which of them the release
/// holds.
fn wanted_files(query: &Query, album: &lidarr::Album) -> Vec<WantedFile> {
    let wanted: Vec<WantedFile> = query
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
        .collect();

    let Some(tracks) = tracklist(album) else {
        return wanted;
    };
    let titles: Vec<String> = tracks
        .iter()
        .filter_map(|track| track.title.as_deref())
        .map(normalize)
        .filter(|title| !title.is_empty())
        .collect();

    if titles.is_empty() {
        return wanted;
    }

    wanted
        .into_iter()
        .filter(|file| titles.contains(&normalize(&file.title)))
        .collect()
}

/// Returns the tracklist that describes the album, when Lidarr sent one.
///
/// # Parameters
///
/// - `album`: the lookup result.
///
/// # Returns
///
/// The tracks of the release Lidarr monitors, or of the release with the most
/// tracks when none is monitored. `None` when no release carries a tracklist.
fn tracklist(album: &lidarr::Album) -> Option<&[lidarr::ReleaseTrack]> {
    let mut best: Option<&lidarr::Release> = None;

    for release in &album.releases {
        if release.tracks.is_empty() {
            continue;
        }

        best = match best {
            None => Some(release),
            Some(current) => {
                let more_complete = (release.monitored, release.tracks.len())
                    > (current.monitored, current.tracks.len());

                Some(if more_complete { release } else { current })
            }
        };
    }

    best.map(|release| release.tracks.as_slice())
}

/// Normalizes a name so two spellings of it compare equal.
///
/// # Parameters
///
/// - `value`: the name to normalize.
///
/// # Returns
///
/// NFKD-decomposed, lowercase, with combining marks dropped, every
/// non-alphanumeric character turned into a space, whitespace collapsed and
/// both ends trimmed.
///
/// # Example
///
/// ```text
/// normalize("Cry Baby (Deluxe Edition)") -> "cry baby deluxe edition"
/// normalize("Beyonc\u{e9}") -> "beyonce"
/// ```
pub fn normalize(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut pending_space = false;

    for character in value.nfkd() {
        if unicode_normalization::char::is_combining_mark(character) {
            continue;
        }

        for lowered in character.to_lowercase() {
            if lowered.is_alphanumeric() {
                if pending_space && !normalized.is_empty() {
                    normalized.push(' ');
                }
                pending_space = false;
                normalized.push(lowered);
            } else {
                pending_space = true;
            }
        }
    }

    normalized
}

/// Removes a trailing edition qualifier from a title.
///
/// # Parameters
///
/// - `title`: the album title.
///
/// # Returns
///
/// The title without its trailing `(...)` or `[...]` groups that name an
/// edition variant, such as `(Deluxe Edition)` or `[Remastered]`, and unchanged
/// otherwise.
///
/// # Example
///
/// ```text
/// without_edition("Cry Baby (Deluxe Edition)") -> "Cry Baby"
/// without_edition("Cry Baby (Live)") -> "Cry Baby (Live)"
/// ```
pub fn without_edition(title: &str) -> String {
    let mut value = title.trim();

    while let Some(stripped) = strip_trailing_qualifier(value) {
        value = stripped.trim_end();
    }

    value.to_string()
}

/// Strips one trailing bracket group when it is an edition qualifier.
///
/// # Parameters
///
/// - `value`: the title, without trailing whitespace.
///
/// # Returns
///
/// The title before the group, and `None` when the title does not end with a
/// group naming an edition variant.
fn strip_trailing_qualifier(value: &str) -> Option<&str> {
    let trimmed = value.trim_end();
    let end = trimmed.len();
    let mut depth = 0usize;
    let mut opened = None;
    let mut trailing = None;

    for (offset, character) in trimmed.char_indices() {
        match character {
            '(' | '[' => {
                if depth == 0 {
                    opened = Some((offset, character));
                }
                depth += 1;
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);

                if depth == 0 {
                    if offset + character.len_utf8() == end {
                        trailing = opened;
                    }
                    opened = None;
                }
            }
            _ => {}
        }
    }

    let (start, opener) = trailing?;
    let closer = if opener == '(' { ')' } else { ']' };
    let content = &trimmed[start + opener.len_utf8()..end - closer.len_utf8()];

    is_edition_qualifier(content).then(|| &trimmed[..start])
}

/// Reports whether a bracket group names an edition variant.
///
/// # Parameters
///
/// - `content`: the text inside the brackets.
///
/// # Returns
///
/// `true` when one of its words is an edition word such as `deluxe` or
/// `remastered`.
fn is_edition_qualifier(content: &str) -> bool {
    normalize(content)
        .split(' ')
        .any(|word| EDITION_WORDS.contains(&word))
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
/// The candidate record, with one file per listed track.
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
            namespace: "mb-release-group".to_string(),
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

    fn mapping() -> quality::Mapping {
        quality::Mapping {
            format: "flac".to_string(),
            bitrate_kbps: None,
        }
    }

    fn cry_baby_query() -> Query {
        let titles = [
            "Cry Baby",
            "Dollhouse",
            "Sippy Cup",
            "Carousel",
            "Alphabet Boy",
            "Soap",
            "Training Wheels",
            "Pity Party",
            "Tag, You're It",
            "Milk and Cookies",
            "Pacify Her",
            "Mrs. Potato Head",
            "Play Date",
            "Teddy Bear",
            "Cake",
            "Dollhouse (Acoustic)",
        ];

        Query {
            granularity: Granularity::Album,
            album: Some("Cry Baby (Deluxe Edition)".to_string()),
            album_artist: Some("Melanie Martinez".to_string()),
            year: Some(2015),
            tracks: titles
                .iter()
                .enumerate()
                .map(|(index, title)| WantedTrack {
                    title: title.to_string(),
                    artists: vec!["Melanie Martinez".to_string()],
                    track_number: Some(index as u16 + 1),
                    disc_number: Some(1),
                    duration_ms: Some(200_000),
                    isrc: None,
                    ids: Vec::new(),
                })
                .collect(),
            preferred_formats: vec!["flac".to_string()],
        }
    }

    #[test]
    fn offers_only_the_requested_album_from_a_live_lookup() {
        let albums: Vec<lidarr::Album> = fixtures::json("lookup_melanie.json");
        let found = candidates(&cry_baby_query(), &albums, &mapping());

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].album_foreign_id,
            "85dd3078-58e9-46fc-b1ae-7d3abfde48c3"
        );
        assert_eq!(found[0].album_title, "Cry Baby");
        assert_eq!(found[0].artist_name, "Melanie Martinez");
        assert_eq!(found[0].files.len(), 16);
    }

    #[test]
    fn matches_case_accents_and_edition_qualifiers() {
        let albums: Vec<lidarr::Album> = fixtures::json("lookup_melanie.json");
        let mut query = cry_baby_query();
        query.album = Some("cry baby [deluxe edition]".to_string());
        query.album_artist = Some("Mel\u{e1}nie Mart\u{ed}nez".to_string());

        assert_eq!(candidates(&query, &albums, &mapping()).len(), 1);
    }

    #[test]
    fn lists_the_wanted_tracks_the_monitored_release_holds() {
        let albums: Vec<lidarr::Album> = fixtures::json("lookup_tracks.json");
        let found = candidates(&cry_baby_query(), &albums, &mapping());

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].files.len(), 13);
        assert_eq!(found[0].files[0].title, "Cry Baby");
        assert_eq!(found[0].files[12].title, "Play Date");
        assert!(found[0].files.iter().all(|file| file.title != "Teddy Bear"));
    }

    #[test]
    fn lists_the_deluxe_tracklist_when_lidarr_monitors_it() {
        let mut albums: Vec<lidarr::Album> = fixtures::json("lookup_tracks.json");
        albums[0].releases[0].monitored = false;
        albums[0].releases[1].monitored = true;

        let found = candidates(&cry_baby_query(), &albums, &mapping());

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].files.len(), 16);
        assert_eq!(found[0].files[15].title, "Dollhouse (Acoustic)");
    }

    #[test]
    fn drops_a_result_whose_tracklist_shares_no_wanted_title() {
        let mut albums: Vec<lidarr::Album> = fixtures::json("lookup_tracks.json");

        for release in &mut albums[0].releases {
            for track in &mut release.tracks {
                track.title = Some("Something Else".to_string());
            }
        }

        assert!(candidates(&cry_baby_query(), &albums, &mapping()).is_empty());
    }

    #[test]
    fn normalizes_case_accents_and_punctuation() {
        assert_eq!(normalize("Beyonc\u{e9}"), "beyonce");
        assert_eq!(normalize("  Don\u{2019}t  Cry, Baby! "), "don t cry baby");
        assert_eq!(normalize("AC/DC"), "ac dc");
        assert_eq!(normalize("---"), "");
    }

    #[test]
    fn strips_only_edition_qualifiers_from_a_title() {
        assert_eq!(without_edition("Cry Baby (Deluxe Edition)"), "Cry Baby");
        assert_eq!(without_edition("Cry Baby [Remastered]"), "Cry Baby");
        assert_eq!(without_edition("Cry Baby (2015 Remaster)"), "Cry Baby");
        assert_eq!(
            without_edition("Cry Baby (Deluxe Edition) [Remastered]"),
            "Cry Baby"
        );
        assert_eq!(without_edition("Cry Baby (Live)"), "Cry Baby (Live)");
        assert_eq!(without_edition("Cry Baby"), "Cry Baby");
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
