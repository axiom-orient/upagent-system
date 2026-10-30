//! Content digests shared by artifact, publication, and compiler evidence.

use sha2::{Digest, Sha256};

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Syntactic check that `value` is a lowercase 64-character SHA-256 hex digest.
///
/// This is a *format* check only. The embedded vector authority deliberately uses its own
/// stricter validator that additionally rejects an all-zero digest, because there an unset
/// placeholder must never pass as verified authority; do not replace one with the other.
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn streaming_matches_whole_buffer() {
        let input = b"the quick brown fox jumps over the lazy dog".repeat(3);
        let mut streamed = Sha256::new();
        for chunk in input.chunks(7) {
            streamed.update(chunk);
        }
        let digest: [u8; 32] = streamed.finalize().into();
        assert_eq!(digest, sha256(&input));
    }
}
