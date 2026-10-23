//! JSON fixtures captured from Lidarr's published API shapes.
//!
//! The fixtures are compiled into the test binary, so the unit tests never
//! touch the network or the filesystem.

use serde::de::DeserializeOwned;

/// Returns the contents of one fixture.
///
/// # Parameters
///
/// - `name`: the fixture's file name.
///
/// # Returns
///
/// The file's text.
///
/// # Panics
///
/// Panics on an unknown name, which is a bug in the test that asked.
pub fn text(name: &str) -> &'static str {
    match name {
        "lookup.json" => include_str!("fixtures/lookup.json"),
        "lookup_melanie.json" => include_str!("fixtures/lookup_melanie.json"),
        "lookup_tracks.json" => include_str!("fixtures/lookup_tracks.json"),
        "quality_profile.json" => include_str!("fixtures/quality_profile.json"),
        "queue.json" => include_str!("fixtures/queue.json"),
        "track_files.json" => include_str!("fixtures/track_files.json"),
        "history_failed.json" => include_str!("fixtures/history_failed.json"),
        other => panic!("unknown fixture {other}"),
    }
}

/// Deserializes one fixture.
///
/// # Parameters
///
/// - `name`: the fixture's file name.
///
/// # Returns
///
/// The fixture decoded into `T`.
///
/// # Panics
///
/// Panics when the fixture is not JSON for `T`, which is a bug in the test that
/// asked.
pub fn json<T: DeserializeOwned>(name: &str) -> T {
    serde_json::from_str(text(name)).unwrap()
}
