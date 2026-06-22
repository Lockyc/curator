//! Per-(window,tab) WebKit data-store identity. Each tab in each window gets its own 16-byte
//! data-store id, derived from `identity::session_seed(window_id, url)`, so logins are isolated
//! per window (profiles) and survive URL edits within a window.

use crate::hash::fnv1a_64;

fn salted(seed: &str, salt: u8) -> u64 {
    let mut buf = Vec::with_capacity(seed.len() + 1);
    buf.push(salt);
    buf.extend_from_slice(seed.as_bytes());
    fnv1a_64(&buf)
}

/// Derive a stable 16-byte WebKit `data_store_identifier` from a session seed. Two salted
/// FNV-1a passes fill the 16 bytes.
pub fn data_store_id(seed: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&salted(seed, 0).to_le_bytes());
    out[8..].copy_from_slice(&salted(seed, 1).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_same_seed() {
        assert_eq!(
            data_store_id("wA:https://x/"),
            data_store_id("wA:https://x/")
        );
    }

    #[test]
    fn distinct_seeds_give_distinct_stores() {
        assert_ne!(
            data_store_id("wA:https://x/"),
            data_store_id("wB:https://x/")
        );
    }

    #[test]
    fn never_collides_with_wrys_default_zero_store() {
        assert_ne!(data_store_id("wA:https://x/"), [0u8; 16]);
    }
}
