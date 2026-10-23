//! The quality a Lidarr profile resolves to, and the candidate format it
//! implies.
//!
//! Lidarr chooses the download's quality itself; the plugin only mirrors the
//! profile's cutoff in its candidate list so the server knows what to expect.
//! The mapping is deliberately small: a profile cutoff names one of Lidarr's
//! qualities, and the plugin recognizes the ones that carry a meaningful audio
//! format and bitrate.
//!
//! Everything here is pure: it never touches the network, so it is unit-tested
//! on the host target.

use crate::lidarr;

/// The format a candidate file is expected to have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    /// The audio codec, for example `flac` or `mp3`.
    pub format: String,
    /// The bitrate in kilobits per second, when the quality names one.
    pub bitrate_kbps: Option<u32>,
}

/// Resolves the format of a quality profile's cutoff.
///
/// # Parameters
///
/// - `profile`: the quality profile the instance is configured with.
///
/// # Returns
///
/// The mapping of the quality the cutoff names. A profile whose cutoff is not
/// listed, or whose name the plugin does not recognize, maps to FLAC, which is
/// what Lidarr's own default profile targets and what most of its downloads
/// deliver.
///
/// # Example
///
/// ```text
/// mapping(profile_with_cutoff("MP3-320"))
///     -> Mapping { format: "mp3", bitrate_kbps: Some(320) }
/// ```
pub fn mapping(profile: &lidarr::QualityProfile) -> Mapping {
    quality_name(profile)
        .as_deref()
        .map_or_else(default_mapping, codec)
}

/// Maps one Lidarr quality name to a format and bitrate.
///
/// # Parameters
///
/// - `name`: the quality name, for example `FLAC`, `MP3-320` or `AAC-256`.
///
/// # Returns
///
/// `flac` for FLAC and for every name the plugin does not recognize, `mp3` or
/// `aac` for those families, with the bitrate when the name carries one. A
/// quality name without a numeric bitrate, such as `MP3-V0`, maps to its codec
/// without a bitrate.
///
/// # Example
///
/// ```text
/// codec("MP3-320") -> Mapping { format: "mp3", bitrate_kbps: Some(320) }
/// codec("FLAC")    -> Mapping { format: "flac", bitrate_kbps: None }
/// ```
pub fn codec(name: &str) -> Mapping {
    let name = name.trim().to_ascii_uppercase();

    if name.starts_with("FLAC") {
        return Mapping {
            format: "flac".to_string(),
            bitrate_kbps: None,
        };
    }

    for (prefix, format) in [("MP3", "mp3"), ("AAC", "aac")] {
        if name.starts_with(prefix) {
            return Mapping {
                format: format.to_string(),
                bitrate_kbps: bitrate(&name),
            };
        }
    }

    default_mapping()
}

/// Returns the name of the quality a profile's cutoff names.
///
/// A cutoff may name a specific quality or a group of them; a group resolves to
/// the first quality it contains.
///
/// # Parameters
///
/// - `profile`: the quality profile.
///
/// # Returns
///
/// The quality name, and `None` when the cutoff names no listed entry.
///
/// # Example
///
/// ```text
/// quality_name(profile_with_cutoff("MP3-320")) -> Some("MP3-320")
/// ```
pub fn quality_name(profile: &lidarr::QualityProfile) -> Option<String> {
    find_quality_name(&profile.items, profile.cutoff)
}

/// Searches one level of a profile's items for the cutoff.
///
/// # Parameters
///
/// - `items`: the profile's entries.
/// - `cutoff`: the quality or group id the cutoff names.
///
/// # Returns
///
/// The matching quality name.
fn find_quality_name(items: &[lidarr::QualityProfileItem], cutoff: i64) -> Option<String> {
    for item in items {
        if let Some(quality) = &item.quality
            && quality.id == cutoff
            && let Some(name) = &quality.name
        {
            return Some(name.clone());
        }

        if i64::from(item.id) == cutoff
            && let Some(name) = first_quality_name(&item.items)
        {
            return Some(name);
        }

        if let Some(name) = find_quality_name(&item.items, cutoff) {
            return Some(name);
        }
    }

    None
}

/// Returns the first quality name inside a group.
///
/// # Parameters
///
/// - `items`: the group's entries.
///
/// # Returns
///
/// The first entry's quality name.
fn first_quality_name(items: &[lidarr::QualityProfileItem]) -> Option<String> {
    items.iter().find_map(|item| {
        item.quality
            .as_ref()
            .and_then(|quality| quality.name.clone())
            .or_else(|| first_quality_name(&item.items))
    })
}

/// Returns the bitrate a quality name carries.
///
/// # Parameters
///
/// - `name`: the uppercased quality name.
///
/// # Returns
///
/// The first part after a `-` that is a number, if any.
fn bitrate(name: &str) -> Option<u32> {
    name.split('-').find_map(|part| part.parse().ok())
}

/// Returns the mapping used when no name is known.
fn default_mapping() -> Mapping {
    Mapping {
        format: "flac".to_string(),
        bitrate_kbps: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn profile(name: &str) -> lidarr::QualityProfile {
        let id = 7;
        lidarr::QualityProfile {
            id: 1,
            cutoff: id,
            items: vec![lidarr::QualityProfileItem {
                id: 0,
                quality: Some(lidarr::Quality {
                    id,
                    name: Some(name.to_string()),
                }),
                items: Vec::new(),
            }],
        }
    }

    #[test]
    fn maps_the_fixture_cutoff() {
        let profile: lidarr::QualityProfile = fixtures::json("quality_profile.json");

        assert_eq!(
            mapping(&profile),
            Mapping {
                format: "flac".to_string(),
                bitrate_kbps: None,
            }
        );
    }

    #[test]
    fn maps_every_recognized_quality_family() {
        assert_eq!(codec("FLAC").format, "flac");
        assert_eq!(codec("MP3-320").format, "mp3");
        assert_eq!(codec("MP3-320").bitrate_kbps, Some(320));
        assert_eq!(codec("AAC-256").format, "aac");
        assert_eq!(codec("AAC-256").bitrate_kbps, Some(256));
    }

    #[test]
    fn keeps_a_codec_without_naming_a_bitrate() {
        assert_eq!(
            codec("MP3-V0"),
            Mapping {
                format: "mp3".to_string(),
                bitrate_kbps: None,
            }
        );
    }

    #[test]
    fn falls_back_to_flac_for_unknown_qualities() {
        assert_eq!(codec("ALAC"), default_mapping());
        assert_eq!(codec(""), default_mapping());
    }

    #[test]
    fn resolves_a_group_cutoff_to_its_first_quality() {
        let profile = lidarr::QualityProfile {
            id: 1,
            cutoff: 9,
            items: vec![lidarr::QualityProfileItem {
                id: 9,
                quality: None,
                items: vec![lidarr::QualityProfileItem {
                    id: 0,
                    quality: Some(lidarr::Quality {
                        id: 2,
                        name: Some("MP3-320".to_string()),
                    }),
                    items: Vec::new(),
                }],
            }],
        };

        assert_eq!(mapping(&profile).format, "mp3");
    }

    #[test]
    fn resolves_a_named_profile() {
        assert_eq!(mapping(&profile("MP3-320")).bitrate_kbps, Some(320));
        assert_eq!(mapping(&profile("AAC-192")).format, "aac");
    }
}
