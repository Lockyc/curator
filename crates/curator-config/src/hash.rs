//! Vendored 64-bit FNV-1a. Used to derive the window id, webview labels, and per-(window,tab)
//! WebKit `data_store_identifier`.
//!
//! Deliberately *not* `std`'s `DefaultHasher`: std does not guarantee that algorithm is stable
//! across Rust releases. These hashes are baked into login-bearing identity and curator is
//! rebuilt from source with whatever toolchain the user has. A `rustc` bump that reshuffled
//! hashing would silently remap every service onto a fresh, empty store — logging the user out
//! of everything. FNV-1a is frozen here; the test vectors lock it in place.

const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// 64-bit FNV-1a hash of `bytes`.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_fnv1a_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn distinct_inputs_differ() {
        assert_ne!(fnv1a_64(b"element"), fnv1a_64(b"gmail"));
    }
}
