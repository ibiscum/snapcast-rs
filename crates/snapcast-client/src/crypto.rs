//! ChaCha20-Poly1305 decryption for audio chunks.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use snapcast_proto::f32lz4::{CRYPTO_HKDF_INFO, CRYPTO_KEY_LEN, CRYPTO_NONCE_LEN, CRYPTO_TAG_LEN};

fn derive_key(psk: &[u8], salt: &[u8]) -> [u8; CRYPTO_KEY_LEN] {
    let hk = Hkdf::<Sha256>::new(Some(salt), psk);
    let mut key = [0u8; CRYPTO_KEY_LEN];
    hk.expand(CRYPTO_HKDF_INFO, &mut key)
        .expect("CRYPTO_KEY_LEN is a valid HKDF-SHA256 output length");
    key
}

/// Audio chunk decryptor.
pub struct ChunkDecryptor {
    cipher: ChaCha20Poly1305,
}

impl ChunkDecryptor {
    /// Create from PSK and session salt.
    pub fn new(psk: &str, salt: &[u8]) -> Self {
        let key = derive_key(psk.as_bytes(), salt);
        Self {
            cipher: ChaCha20Poly1305::new(&key.into()),
        }
    }

    /// Decrypt a chunk. Input: `[nonce][ciphertext + tag]`.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, chacha20poly1305::Error> {
        if data.len() < CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN {
            return Err(chacha20poly1305::Error);
        }
        let nonce = Nonce::from_slice(&data[..CRYPTO_NONCE_LEN]);
        self.cipher.decrypt(nonce, &data[CRYPTO_NONCE_LEN..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypt_chunk(
        psk: &str,
        salt: &[u8],
        nonce: [u8; CRYPTO_NONCE_LEN],
        plaintext: &[u8],
    ) -> Vec<u8> {
        let key = derive_key(psk.as_bytes(), salt);
        let cipher = ChaCha20Poly1305::new(&key.into());
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .expect("test encryption should succeed");
        let mut out = Vec::with_capacity(CRYPTO_NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        out
    }

    #[test]
    fn derive_key_is_deterministic_and_salt_sensitive() {
        let psk = b"my-psk";
        let salt_a = b"salt-a";
        let salt_b = b"salt-b";
        let key1 = derive_key(psk, salt_a);
        let key2 = derive_key(psk, salt_a);
        let key3 = derive_key(psk, salt_b);
        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn derive_key_matches_known_hkdf_vector() {
        let key = derive_key(b"my-psk", b"salt-a");
        let expected: [u8; CRYPTO_KEY_LEN] = [
            0xc6, 0xec, 0xb2, 0x02, 0x8a, 0xe1, 0x54, 0x6e, 0x65, 0xe1, 0x40, 0xb9, 0x53, 0xb7,
            0xc7, 0x2b, 0x10, 0x83, 0x27, 0x8f, 0xec, 0xad, 0x60, 0xf3, 0xc3, 0x19, 0x34, 0x4c,
            0x9d, 0x73, 0x2e, 0xa4,
        ];
        assert_eq!(key, expected);
    }

    #[test]
    fn decrypt_roundtrip_succeeds() {
        let psk = "key";
        let salt = b"salt";
        let nonce = [7u8; CRYPTO_NONCE_LEN];
        let plaintext = b"snapcast-audio-frame";
        let chunk = encrypt_chunk(psk, salt, nonce, plaintext);

        let dec = ChunkDecryptor::new(psk, salt);
        let out = dec.decrypt(&chunk).unwrap();
        assert_eq!(out, plaintext);
    }

    #[test]
    fn decrypt_roundtrip_empty_plaintext_succeeds() {
        let psk = "key";
        let salt = b"salt";
        let nonce = [3u8; CRYPTO_NONCE_LEN];
        let chunk = encrypt_chunk(psk, salt, nonce, b"");

        let dec = ChunkDecryptor::new(psk, salt);
        let out = dec.decrypt(&chunk).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn decrypt_short_input_fails() {
        let dec = ChunkDecryptor::new("key", b"salt");
        for len in [
            0,
            CRYPTO_NONCE_LEN - 1,
            CRYPTO_NONCE_LEN,
            CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN - 1,
        ] {
            assert!(
                dec.decrypt(&vec![0u8; len]).is_err(),
                "length {len} should be rejected"
            );
        }
    }

    #[test]
    fn decrypt_exact_minimum_length_is_rejected() {
        let dec = ChunkDecryptor::new("key", b"salt");
        let frame = vec![0u8; CRYPTO_NONCE_LEN + CRYPTO_TAG_LEN];
        assert!(dec.decrypt(&frame).is_err());
    }

    #[test]
    fn decrypt_with_tampered_nonce_fails() {
        let psk = "key";
        let salt = b"salt";
        let nonce = [9u8; CRYPTO_NONCE_LEN];
        let plaintext = b"hello";
        let mut chunk = encrypt_chunk(psk, salt, nonce, plaintext);
        chunk[0] ^= 0x01;

        let dec = ChunkDecryptor::new(psk, salt);
        assert!(dec.decrypt(&chunk).is_err());
    }

    #[test]
    fn decrypt_with_tampered_ciphertext_fails() {
        let psk = "key";
        let salt = b"salt";
        let nonce = [9u8; CRYPTO_NONCE_LEN];
        let plaintext = b"hello";
        let mut chunk = encrypt_chunk(psk, salt, nonce, plaintext);
        chunk[CRYPTO_NONCE_LEN] ^= 0x01;

        let dec = ChunkDecryptor::new(psk, salt);
        assert!(dec.decrypt(&chunk).is_err());
    }
        let nonce = random_nonce();
    #[test]
    fn decrypt_with_tampered_tag_fails() {
        let psk = "key";
        let salt = b"salt";
        let nonce = [8u8; CRYPTO_NONCE_LEN];
        let plaintext = b"tag-check";
        let mut chunk = encrypt_chunk(psk, salt, nonce, plaintext);
        let last = chunk.len() - 1;
        chunk[last] ^= 0x01;

        let dec = ChunkDecryptor::new(psk, salt);
        assert!(dec.decrypt(&chunk).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let salt = b"salt";
        let nonce = [1u8; CRYPTO_NONCE_LEN];
        let plaintext = b"authentic-frame";
        let chunk = encrypt_chunk("right-key", salt, nonce, plaintext);
        let dec = ChunkDecryptor::new("wrong-key", salt);
        assert!(dec.decrypt(&chunk).is_err());
    }

    #[test]
    fn wrong_salt_fails() {
        let nonce = random_nonce();
        let plaintext = b"authentic-frame";
        let chunk = encrypt_chunk("same-key", b"right-salt", nonce, plaintext);
        let dec = ChunkDecryptor::new("same-key", b"wrong-salt");
        assert!(dec.decrypt(&chunk).is_err());
    }

    #[test]
    fn empty_psk_does_not_share_key_with_nonempty_psk() {
        let salt = b"shared-salt";
        let empty_key = derive_key(b"", salt);
        let nonempty_key = derive_key(b"key", salt);
        assert_ne!(empty_key, nonempty_key);

        let nonce = [5u8; CRYPTO_NONCE_LEN];
        let plaintext = b"empty-key-check";
        let chunk = encrypt_chunk("", salt, nonce, plaintext);
        let empty_dec = ChunkDecryptor::new("", salt);
        assert_eq!(empty_dec.decrypt(&chunk).unwrap(), plaintext);

        let wrong_dec = ChunkDecryptor::new("key", salt);
        assert!(wrong_dec.decrypt(&chunk).is_err());
    }
}
