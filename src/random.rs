//! Random keys for the plugin's own store entries.
//!
//! A search and a download are named by a random key, so the token the server
//! holds cannot collide with another call's state. The key is a lowercase
//! hyphen-free UUID, which is safe in a token and in a store key.
//!
//! Everything here is pure: it never touches the network, so it is unit-tested
//! on the host target.

/// Returns a new random key.
///
/// # Returns
///
/// A 32-character lowercase hexadecimal string.
///
/// # Example
///
/// ```text
/// key() -> "3f2b1c0d9e8a7b6c5d4e3f2a1b0c9d8e"
/// ```
pub fn key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn returns_distinct_hex_keys() {
        let keys: HashSet<String> = (0..100).map(|_| key()).collect();

        assert_eq!(keys.len(), 100);
        assert!(keys.iter().all(|key| key.len() == 32));
        assert!(
            keys.iter()
                .all(|key| key.chars().all(|character| character.is_ascii_hexdigit()))
        );
    }
}
