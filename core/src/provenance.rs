//! Provenance utilities — hashing helpers that are platform-independent.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Hash a string using the standard library's `DefaultHasher` and return a
/// 16-character lowercase hex string.  Not cryptographically strong, but
/// adequate for provenance identification.
pub fn hash_string(s: &str) -> String {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_string_is_16_hex_chars() {
        let h = hash_string("hello world");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_string_same_input_same_output() {
        assert_eq!(hash_string("task-1"), hash_string("task-1"));
    }

    #[test]
    fn hash_string_different_inputs_different_outputs() {
        assert_ne!(hash_string("task-1"), hash_string("task-2"));
    }

    #[test]
    fn hash_empty_string() {
        let h = hash_string("");
        assert_eq!(h.len(), 16);
    }
}
