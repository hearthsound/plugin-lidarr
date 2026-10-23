//! The opaque token codec.
//!
//! A plugin instance is recycled between calls, so every later call has to be
//! given back whatever the earlier one learned. The plugin does that with two
//! mechanisms: the token the server holds, which names the host-kv entry
//! holding the rest of the state, and the token's own payload for the parts
//! that are small enough to travel with it.
//!
//! A token is the base64url encoding, without padding, of a JSON object, so it
//! survives URLs, log lines and database columns unchanged.
//!
//! Everything here is pure: it never touches the network, so it is unit-tested
//! on the host target.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hearthsound_plugin::types::ProviderError;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Encodes a token payload.
///
/// # Parameters
///
/// - `value`: the payload to encode.
///
/// # Returns
///
/// The base64url token, without padding.
///
/// # Errors
///
/// Returns [`ProviderError::Internal`] when the payload cannot be serialized,
/// which for the payloads of this plugin means a bug rather than a bad call.
pub fn encode<T: Serialize>(value: &T) -> Result<String, ProviderError> {
    let json = serde_json::to_vec(value)
        .map_err(|error| ProviderError::Internal(format!("token: {error}")))?;

    Ok(URL_SAFE_NO_PAD.encode(json))
}

/// Decodes a token into its payload.
///
/// # Parameters
///
/// - `token`: the token a caller handed back.
///
/// # Returns
///
/// The decoded payload.
///
/// # Errors
///
/// Returns [`ProviderError::Internal`] when the token is not base64url or not
/// JSON for `T`, which means the server handed back a token this instance did
/// not issue.
pub fn decode<T: DeserializeOwned>(token: &str) -> Result<T, ProviderError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(token.trim())
        .map_err(|error| ProviderError::Internal(format!("token: {error}")))?;

    serde_json::from_slice(&bytes)
        .map_err(|error| ProviderError::Internal(format!("token: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Payload {
        album_id: u32,
        title: String,
    }

    #[test]
    fn round_trips_a_payload() {
        let payload = Payload {
            album_id: 7,
            title: "Kid A".to_string(),
        };
        let token = encode(&payload).unwrap();

        assert!(!token.contains('='), "{token}");
        assert_eq!(decode::<Payload>(&token).unwrap(), payload);
    }

    #[test]
    fn rejects_a_token_it_did_not_issue() {
        assert!(decode::<Payload>("not a token").is_err());
        assert!(decode::<Payload>(&encode(&"hello").unwrap()).is_err());
    }
}
