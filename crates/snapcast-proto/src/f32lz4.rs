//! Wire-format constants for the `f32lz4` / `f32lz4e` (encrypted) codec.
//!
//! Shared constants for the byte-level contract that the server encoder
//! (`snapcast-server`) and the client decoder (`snapcast-client`) must agree
//! on exactly. These values were previously duplicated as literals in both
//! crates (and inside the crypto modules), where they could drift apart with
//! no compile error — a drift would surface only as a silent decode/decrypt
//! failure at runtime. Keep shared wire/crypto constants here so both ends
//! reference the same source.

#[cfg(feature = "encryption")]
use hkdf::Hkdf;
#[cfg(feature = "encryption")]
use sha2::Sha256;

/// Codec-header magic identifying an `f32lz4` stream.
pub const F32LZ4_MAGIC: &[u8; 4] = b"F32L";

/// Length of the base `f32lz4` codec header in bytes.
///
/// Layout: `MAGIC(4) + sample_rate: u32(4) + channels: u16(2) + bits: u16(2)`.
pub const F32LZ4_HEADER_LEN: usize = 12;

/// Marker placed after the base header to signal an encrypted stream.
///
/// When present it is immediately followed by [`F32LZ4_SALT_LEN`] salt bytes.
pub const F32LZ4_ENC_MARKER: &[u8; 4] = b"ENC\0";

/// Length of the per-session encryption salt, in bytes.
pub const F32LZ4_SALT_LEN: usize = 16;

/// Total codec-header length when encryption is enabled.
///
/// `base header + ENC marker (4) + salt`.
pub const F32LZ4_ENC_HEADER_LEN: usize =
    F32LZ4_HEADER_LEN + F32LZ4_ENC_MARKER.len() + F32LZ4_SALT_LEN;

// --- ChaCha20-Poly1305 AEAD parameters (key derived via HKDF-SHA256) ---

/// AEAD nonce length for ChaCha20-Poly1305, in bytes.
pub const CRYPTO_NONCE_LEN: usize = 12;

/// AEAD authentication tag length (Poly1305), in bytes.
pub const CRYPTO_TAG_LEN: usize = 16;

/// Derived symmetric key length (ChaCha20 256-bit key), in bytes.
pub const CRYPTO_KEY_LEN: usize = 32;

/// HKDF `info` context string binding the derived key to this codec.
///
/// Both ends must expand with the identical info string or the derived keys
/// differ and every chunk fails to authenticate.
pub const CRYPTO_HKDF_INFO: &[u8] = b"snapcast-f32lz4e";

/// Derives a 256-bit encryption key from a PSK and salt via HKDF-SHA256.
#[cfg(feature = "encryption")]
pub fn derive_encryption_key(psk: &[u8], salt: &[u8]) -> [u8; CRYPTO_KEY_LEN] {
    let hk = Hkdf::<Sha256>::new(Some(salt), psk);
    let mut key = [0u8; CRYPTO_KEY_LEN];
    hk.expand(CRYPTO_HKDF_INFO, &mut key)
        .expect("CRYPTO_KEY_LEN is a valid HKDF-SHA256 output length");
    key
}

/// Build a ChaCha20-Poly1305 nonce from a little-endian chunk counter.
///
/// Layout: `[counter_le_u64][0u8; 4]`.
#[cfg(feature = "encryption")]
pub fn make_counter_nonce(counter: u64) -> [u8; CRYPTO_NONCE_LEN] {
    let mut nonce = [0u8; CRYPTO_NONCE_LEN];
    nonce[..8].copy_from_slice(&counter.to_le_bytes());
    nonce
}

/// Split an encrypted chunk into nonce and ciphertext-with-tag sections.
///
/// Returns `None` when `data` is shorter than `nonce + tag`.
#[cfg(feature = "encryption")]
pub fn split_encrypted_chunk(data: &[u8]) -> Option<([u8; CRYPTO_NONCE_LEN], &[u8])> {
    if data.len() < CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN {
        return None;
    }
    let mut nonce = [0u8; CRYPTO_NONCE_LEN];
    nonce.copy_from_slice(&data[..CRYPTO_NONCE_LEN]);
    Some((nonce, &data[CRYPTO_NONCE_LEN..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_constants_match_wire_layout() {
        assert_eq!(F32LZ4_MAGIC.len(), 4);
        assert_eq!(F32LZ4_ENC_MARKER.len(), 4);
        assert_eq!(
            F32LZ4_HEADER_LEN,
            F32LZ4_MAGIC.len() + 4 + 2 + 2,
            "MAGIC(4) + sample_rate(u32) + channels(u16) + bits(u16)"
        );
        assert_eq!(
            F32LZ4_ENC_HEADER_LEN,
            F32LZ4_HEADER_LEN + F32LZ4_ENC_MARKER.len() + F32LZ4_SALT_LEN
        );
    }

    #[test]
    fn crypto_constants_match_algorithm_contract() {
        assert_eq!(CRYPTO_NONCE_LEN, 12, "ChaCha20-Poly1305 nonce size");
        assert_eq!(CRYPTO_TAG_LEN, 16, "Poly1305 tag size");
        assert_eq!(CRYPTO_KEY_LEN, 32, "ChaCha20 key size");
        assert!(!CRYPTO_HKDF_INFO.is_empty(), "HKDF info must be non-empty");
        assert_eq!(CRYPTO_HKDF_INFO, b"snapcast-f32lz4e");
    }

    #[cfg(feature = "encryption")]
    #[test]
    fn derived_key_is_deterministic_and_salt_sensitive() {
        let key1 = derive_encryption_key(b"my-psk", b"salt-a");
        let key2 = derive_encryption_key(b"my-psk", b"salt-a");
        let key3 = derive_encryption_key(b"my-psk", b"salt-b");
        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[cfg(feature = "encryption")]
    #[test]
    fn counter_nonce_layout_is_stable() {
        let nonce = make_counter_nonce(0x0102_0304_0506_0708);
        assert_eq!(&nonce[..8], &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
        assert_eq!(&nonce[8..], &[0, 0, 0, 0]);
    }

    #[cfg(feature = "encryption")]
    #[test]
    fn split_encrypted_chunk_validates_min_length() {
        assert!(split_encrypted_chunk(&[]).is_none());
        assert!(split_encrypted_chunk(&vec![0; CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN - 1]).is_none());

        let frame = vec![7u8; CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN];
        let (nonce, ct) = split_encrypted_chunk(&frame).expect("valid minimum chunk");
        assert_eq!(nonce, [7u8; CRYPTO_NONCE_LEN]);
        assert_eq!(ct.len(), CRYPTO_TAG_LEN);
    }
}
